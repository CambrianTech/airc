//! Subcommand handlers.
//!
//! Local-substrate commands (`init`, `send`, `listen`, `room`,
//! `peer add`, `peer list`) route through `airc_lib::Airc` — the
//! CLI is a thin client of the same API consumers embed. Closes
//! grievance §5 / Codex audit finding #4.
//!
//! Daemon-host commands construct daemon state directly because they
//! host the service. CLI commands that consume daemon-backed messaging
//! go through `airc_lib::Airc::attach` so apps and CLI share the same
//! SDK surface.
//!
//! `VerificationPolicy::Strict` is the only policy used in CLI paths.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use airc_core::{ClientId, EventId, PeerId, RoomId, TranscriptCursor};
use airc_protocol::{PeerKeyRegistry, VerificationPolicy, HEADER_AIRC_CLIENT};
use futures::stream::StreamExt;

use airc_daemon::{run as run_daemon_server, DaemonRuntimeInfo, DaemonState};
use airc_diagnostics::{
    DiagnosticCode, DiagnosticComponent, DiagnosticEvent, DiagnosticSink, StderrJsonDiagnosticSink,
};
use airc_identity::LocalIdentity;
use airc_ipc::{AddPeerRequest, DaemonClient, RemovePeerRequest, Request, Response};
use airc_lib::{Airc, Headers, HeartbeatTask, PeerSpec, DEFAULT_HEARTBEAT_INTERVAL};
use airc_store::{EventStore, SqliteEventStore};
use airc_trust as peers_store;

/// `init` — open the substrate at `<home>`. `Airc::open` loads or
/// generates the identity, opens the event store, applies any
/// pending migrations, and primes the peer registry. The CLI then
/// prints the local peer's spec so the user can share it.
pub async fn run_init(
    home: &Path,
    agent_name: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = match agent_name {
        Some(agent_name) => Airc::open_as(home, agent_name).await?,
        None => Airc::open(home).await?,
    };
    let current = airc.current_room().await?;
    println!("home:        {}", airc.home().display());
    println!("peer_id:     {}", airc.peer_id());
    println!("client_id:   {}", airc.client_id());
    println!("agent_name:  {}", airc.agent_name());
    println!("room:        {} ({})", current.name, current.channel);
    println!("peer_spec:   {}", airc.peer_spec());
    println!();
    println!(
        "Share peer_spec with peers; enrol theirs via `airc peer add <spec>`. \
         Use `airc room <name>` to switch rooms; `airc msg \"hi\"` sends \
         to the current room."
    );
    Ok(())
}

/// `room` — print current room. `room <name>` — switch to a
/// deterministic room derived from `<name>`.
pub async fn run_room(home: &Path, name: Option<String>) -> Result<(), Box<dyn std::error::Error>> {
    let airc = Airc::open(home).await?;
    match name {
        Some(name) => {
            let next = airc.join(&name).await?;
            println!("switched room: {}", next.name);
            println!("  wire:    {}", next.wire.display());
            println!("  channel: {}", next.channel);
        }
        None => {
            // #270: bare `airc room` lists EVERY subscription, not just the
            // current one. The old current-only display read as "my rooms",
            // which is exactly how a whole afternoon of seam messages sat
            // unread in a subscribed-but-not-current channel while both
            // agents concluded the transport was broken. Membership must be
            // visible to be trusted.
            let current = airc.current_room().await?;
            let set = airc.subscription_set().await?;
            println!("room:    {}", current.name);
            println!("wire:    {}", current.wire.display());
            println!("channel: {}", current.channel);
            let others: Vec<_> = set
                .all()
                .filter(|s| s.name.as_str() != current.name)
                .collect();
            if others.is_empty() {
                println!("subscribed: (only the current room)");
            } else {
                println!(
                    "subscribed ({} more — `airc room <name>` to switch):",
                    others.len()
                );
                for sub in others {
                    println!("  {}  channel {}", sub.name.as_str(), sub.room_id);
                }
            }
        }
    }
    Ok(())
}

/// `doctrine-publish` — read a markdown file (default: AGENTS.md at
/// the git repo root) and publish it as the room's operating
/// doctrine via `Airc::publish_room_doctrine`. Card 2903a8ef slice
/// 2/4 of the engine keystone — gets the "how we work here" contract
/// onto the substrate so attaching agents in any scope load it.
///
/// Version: short SHA-256 prefix of the body bytes. Future tooling
/// can compare versions to detect "doctrine on my scope is stale."
pub async fn run_doctrine_publish(
    home: &Path,
    from_file: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Resolve the source path. Default: `<git-repo-root>/AGENTS.md`.
    let path = match from_file {
        Some(p) => p,
        None => {
            let repo_root = std::process::Command::new("git")
                .args(["rev-parse", "--show-toplevel"])
                .output()?;
            if !repo_root.status.success() {
                return Err(format!(
                    "no --from-file passed and git rev-parse --show-toplevel \
                     failed (not in a git repo?): {}",
                    String::from_utf8_lossy(&repo_root.stderr).trim()
                )
                .into());
            }
            let root = String::from_utf8(repo_root.stdout)?.trim().to_string();
            PathBuf::from(root).join("AGENTS.md")
        }
    };
    let body = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read doctrine file {}: {e}", path.display()))?;

    let version = short_content_hash(body.as_bytes());

    let socket = crate::cli::default_socket_path_in(home);
    let socket = ensure_daemon_running(home, socket, Vec::new()).await?;
    let airc = Airc::attach(home, socket).await?;
    airc.publish_room_doctrine(body.clone(), version.clone())
        .await?;
    println!(
        "doctrine_published: file={file} version={version} bytes={bytes}",
        file = path.display(),
        bytes = body.len(),
    );
    Ok(())
}

/// Short content discriminator — first 12 chars of SHA-256 hex of
/// `body`. Twelve chars are enough to distinguish unrelated revisions
/// of a kilobyte-scale doctrine file (the AGENTS.md target) without
/// pulling in a heavier hash; collisions at this scale are
/// astronomically unlikely and the substrate stores every event so a
/// "version" collision degrades to "two latest with the same tag,"
/// not data loss.
fn short_content_hash(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let hex: String = digest.iter().map(|b| format!("{:02x}", b)).collect();
    hex.chars().take(12).collect()
}

/// `part` — leave a subscribed room without deleting identity, trust,
/// or other room subscriptions.
pub async fn run_part(home: &Path, room: Option<String>) -> Result<(), Box<dyn std::error::Error>> {
    let airc = Airc::open(home).await?;
    let parted = airc.part_channel(room.as_deref()).await?;
    println!("parted:  #{}", parted.name);
    println!("channel: {}", parted.channel);
    Ok(())
}

/// `join` — account-room coordinator entrypoint. With no explicit
/// room, subscribe to `#general` plus the inferred Git owner channel.
/// With a room, join that arbitrary channel and make it default.
pub async fn run_join(home: &Path, room: Option<String>) -> Result<(), Box<dyn std::error::Error>> {
    // Start the machine-singular daemon and attach: join, heartbeat, and
    // the live feed all route through the daemon's router (one path).
    let socket = crate::cli::default_socket_path_in(home);
    let socket = ensure_daemon_running(home, socket, Vec::new()).await?;
    let airc = Airc::attach(home, socket.clone()).await?;
    let runtime_context = crate::runtime_context::RuntimeContext::current();
    match room {
        Some(room) => {
            let joined = airc.join(&room).await?;
            println!("joined:  #{}", joined.name);
            println!("wire:    {}", joined.wire.display());
            println!("channel: {}", joined.channel);
            print_scope_context(home, &joined.wire);
        }
        None => {
            let cwd = std::env::current_dir()?;
            // Card 1eae6f3e: snapshot the durable default BEFORE the
            // context re-infer (raw read — `subscription_set` has no
            // lazy-seed side effects) so a default change is reported
            // loudly instead of silently re-targeting `airc msg`.
            let default_before = airc.subscription_set().await?.default;
            let rooms = airc.join_default_context(cwd).await?;
            let current = airc.current_room().await?;
            println!("joined default account context:");
            for room in rooms {
                println!("  #{} ({})", room.name, room.channel);
            }
            if let Some(before) = default_before {
                if before.as_str() != current.name {
                    eprintln!(
                        "WARNING: default room CHANGED: {} -> #{} — `airc msg` now targets #{}",
                        before.display_with_hash(),
                        current.name,
                        current.name
                    );
                }
            }
            println!("default: #{}", current.name);
            println!("wire:    {}", current.wire.display());
            print_scope_context(home, &current.wire);
        }
    }
    sync_daemon_peers_for_current_rooms(home, socket).await?;
    ensure_runtime_integrations();

    // Card 745e93f0 (slice 4/4 of engine-keystone 2903a8ef): surface
    // the room's operating doctrine to the attaching agent. Agent
    // runner harnesses scrape this region from join stdout and
    // inject it into the agent's system context — the "user is not
    // the engine" fix lands here. Marked with stable BEGIN/END
    // markers so the scrape is unambiguous; silent when the room
    // has no published doctrine.
    if let Ok(Some(doctrine)) = airc.room_doctrine().await {
        println!("--- BEGIN ROOM DOCTRINE (version={}) ---", doctrine.version);
        println!("{}", doctrine.body);
        println!("--- END ROOM DOCTRINE ---");
    }

    let _heartbeat = if runtime_context.should_stream_join() {
        Some(start_join_heartbeat(&airc, home, &runtime_context).await?)
    } else {
        None
    };

    // SOS rides ALONG with the live feed instead of waiting to be remembered.
    //
    // The out-of-band channel exists for the case where the wire is down — and
    // that is precisely the case where nobody thinks to run `airc sos watch`,
    // because the wire being down is not announced, it is INFERRED from silence.
    // On 2026-08-12 two operators sat in that silence for hours; the human
    // relayed between them by hand, and `airc sos` had been merged only that
    // night after sitting unmerged through the exact outage it was built for.
    //
    // So: whenever this node streams a join, it also surfaces peer SOS posts.
    // No flag, no verb to recall, no decision to make while blind. `poll_once`
    // is cursor-gated and self-filtering, so a healthy node sees nothing and
    // pays one `gh` call per interval; a blind one sees its peers.
    //
    // Best-effort and detached ON PURPOSE: SOS is the recovery channel, so it
    // must never be able to take down the feed it backs up. A missing `gh`, an
    // unauthenticated shell, or no SOS gist yet all degrade to "no fallback",
    // which is exactly where we were before this existed.
    let _sos_fallback = runtime_context
        .should_stream_join()
        .then(|| start_sos_fallback(home));

    if runtime_context.should_stream_join() {
        crate::join_feed::run(&airc).await?;
    }
    Ok(())
}

/// Poll the SOS gist alongside the live feed, printing any NEW peer posts.
///
/// Returns a task handle whose drop cancels the poll — it lives exactly as long
/// as the join it accompanies.
fn start_sos_fallback(home: &Path) -> tokio::task::JoinHandle<()> {
    let home = home.to_path_buf();
    tokio::spawn(async move {
        // First poll is delayed: a node that just joined is the LEAST likely to
        // need the fallback, and an immediate `gh` call on every join would tax
        // the healthy path to serve the broken one.
        loop {
            tokio::time::sleep(SOS_FALLBACK_POLL_INTERVAL).await;
            // Errors are swallowed rather than reported every tick: `gh` absent
            // or unauthenticated is a STANDING condition, not an event, and a
            // recurring complaint in a live feed is its own kind of noise. The
            // explicit `airc sos status` says so plainly when asked.
            let _ = crate::sos_commands::poll_fallback_once(&home).await;
        }
    })
}

/// How often a joined node checks the out-of-band channel. Deliberately slow:
/// this is a rendezvous, not a bus, and the healthy case must stay cheap.
const SOS_FALLBACK_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(120);

async fn start_join_heartbeat(
    airc: &Airc,
    home: &Path,
    runtime_context: &crate::runtime_context::RuntimeContext,
) -> Result<HeartbeatTask, Box<dyn std::error::Error>> {
    let scope = join_scope_label(home);
    let runtime = runtime_context.runtime_label().to_string();
    let client_id = runtime_context.client_id().map(ToString::to_string);
    let build = (!crate::build_info::is_unknown()).then(|| crate::build_info::COMMIT_SHORT.into());

    // Card 0bf262eb: populate the coordination signal added in
    // aacf2162. This is the minimum-viable slice — the build SHA
    // stands in for `doctrine_version` (the build tree includes
    // AGENTS.md, so observers can still detect peers on stale
    // doctrine), and the other two fields stay default. A follow-up
    // card refreshes `active_claims` from the board projection on
    // every tick.
    let coordination = airc_lib::CoordinationSignal {
        doctrine_version: build.clone(),
        ..Default::default()
    };

    Ok(airc
        .start_agent_heartbeat_with_coordination(
            runtime,
            client_id,
            Some(scope),
            build,
            DEFAULT_HEARTBEAT_INTERVAL,
            coordination,
        )
        .await?)
}

fn join_scope_label(home: &Path) -> String {
    match std::env::current_dir() {
        Ok(cwd) => cwd.display().to_string(),
        Err(_) => home.display().to_string(),
    }
}

fn ensure_runtime_integrations() {
    match crate::integrations::codex::install::install_hooks_for_default_home_if_present() {
        Ok(report) if report.is_empty() => {}
        Ok(report) => {
            for line in report.lines {
                println!("runtime: {line}");
            }
        }
        Err(error) => {
            eprintln!("airc: Codex hook setup skipped: {error}");
        }
    }
}

/// `version` — print package version + install dir. Distinct from
/// clap's `--version` flag (which only prints the package version)
/// because operators use `airc version` to verify two scopes/tabs
/// are on the same build path, not just the same version string.
///
/// Richer build metadata (commit sha, branch, commit subject) is a
/// follow-up — would need a `build.rs` that captures git state at
/// compile time. For now: package version + binary path is enough
/// to distinguish "are we on the same install."
pub fn run_version() -> Result<(), Box<dyn std::error::Error>> {
    let exe = std::env::current_exe()?;
    let exe_path = exe.canonicalize().unwrap_or(exe);
    println!("  airc {}", crate::build_info::PACKAGE_VERSION);
    println!("  install: {}", exe_path.display());
    if !crate::build_info::is_unknown() {
        println!(
            "  build:   {} on {}",
            crate::build_info::COMMIT_SHORT,
            crate::build_info::BRANCH
        );
    } else {
        println!("  build:   unknown (git unavailable at compile time)");
    }
    Ok(())
}

/// Make sure a daemon serving `home` is reachable; return the socket
/// the caller should attach to.
///
/// The returned socket is USUALLY equal to `socket` — every agent
/// resolving the same `home` finds the existing daemon on the same
/// socket. For sandboxed agents (Codex etc.) whose home-resolved
/// socket has no daemon, the cross-sandbox discovery directory
/// (card 282850c2) is consulted and we route to the project's
/// actual daemon instead of spawning a competing orphan in the
/// agent's tmpdir. If neither path finds an existing daemon, a fresh
/// one is spawned at `socket` and announced for future agents.
pub async fn ensure_daemon_running(
    home: &Path,
    socket: PathBuf,
    _peers: Vec<PeerSpec>,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    // 1. Fast path: home-resolved socket already has a current daemon.
    let client = DaemonClient::new(socket.clone());
    if let Ok(status) = client.status_with_timeout(Duration::from_millis(250)).await {
        if daemon_status_is_current(&status) {
            return Ok(socket);
        }
        let _ = client.stop().await;
        wait_for_daemon_exit(&client, Duration::from_secs(3)).await;
    }

    // 2. Card 282850c2: no daemon at the home-resolved socket. Before
    // spawning, consult cross-sandbox discovery for a daemon serving
    // the SAME project root. Sandboxed agents whose `$HOME` was
    // forced into a tmpdir would otherwise orphan a fresh daemon
    // every invocation. We only auto-attach when the discovered
    // daemon's `home` matches ours — different homes mean different
    // identities, and attaching across them would silently borrow
    // the wrong agent's keys (card a1b4552a was the prior class of
    // this kind of attribution leak). In practice the Codex case
    // DOES match: both agents resolve home from the same project
    // root, so the home values agree.
    let project_root = home.parent().map(Path::to_path_buf);
    if let Some(ref pr) = project_root {
        if let Some(discovered) = crate::discovery::find_for_project(pr) {
            if discovered.home == home {
                let alt = DaemonClient::new(discovered.socket.clone());
                if let Ok(status) = alt.status_with_timeout(Duration::from_millis(250)).await {
                    if daemon_status_is_current(&status) {
                        return Ok(discovered.socket);
                    }
                }
            }
        }
    }

    // 3. Spawn a fresh daemon at the home-resolved socket.
    std::fs::create_dir_all(home)?;
    let log = home.join("airc-daemon.log");
    let stdout = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)?;
    let stderr = stdout.try_clone()?;
    let exe = std::env::current_exe()?;
    // The daemon's HOME must be the home that OWNS the socket, not the
    // caller's scope.
    //
    // `default_socket_path_in` resolves through `machine_account_home`, so a
    // caller in a git-project scope (`<repo>/.airc`) legitimately shares the
    // MACHINE-account socket — one daemon per machine is the design. But this
    // spawn passed the CALLER's `home` through, so whichever scope happened to
    // start the daemon first imposed ITS identity on every scope that later
    // attached to that socket. Nothing detected it: both paths are real, the
    // daemon is healthy, and `doctor` reports a clean bill.
    //
    // Measured on BIGMAMA 2026-08-12, the whole night's blackout in one line:
    //
    //   airc.exe --home \\?\C:\...\development\continuum\.airc \
    //            daemon --socket C:\Users\joelt\.airc\runtime\airc-machine-...sock
    //
    // Downstream, all diagnosed separately as unrelated bugs before the cause
    // was found: messages sent through the machine socket were served by the
    // PROJECT identity, so they landed in a scope the intended peer was not
    // enrolled in (a one-way mirror — our sends left, their replies could not
    // arrive); the wrong scope means a different `peer_id`, hence a different
    // `stable_lan_port`, so the advertised endpoint moved and peers' stored
    // endpoints went stale (read from the far side as "SYN dropped / stale
    // ports / firewall"); and the event store read as a monologue because it
    // was the other scope's store.
    //
    // ONE rule, and now ONE implementation of it: `airc_lib::daemon_command`
    // is the only place a daemon's `--home` is chosen. It was previously
    // derived here and, separately, at `update_commands::daemon_command` —
    // where it was missed, which is how `airc update` started taking nodes
    // dark (#1352). A rule with two implementations has one that is wrong.
    let mut command = airc_lib::daemon_command(&exe, home, "daemon", &socket);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    inject_gh_token(&mut command);
    detach_daemon(&mut command);
    let child = command.spawn()?;
    let daemon_pid = child.id();

    let client = DaemonClient::new(socket.clone());
    // Card 7e3c9a1f / #1210: a freshly-spawned daemon must BIND its IPC
    // socket within this window or the CLI gives up, returns an error, and
    // abandons the half-started daemon — which on a fresh/cold machine
    // (first-ever run: SQLite migrations + identity gen + the substrate
    // handle's own `Airc::open` migrations on the daemon's boot path) takes
    // well over the old 5s. M5's repro: the daemon NEVER reached its bind
    // before the 5s reap, so a persistent daemon never formed and every
    // command re-spawned + re-hung. 20s comfortably covers a cold boot
    // while still surfacing a genuinely dead daemon. (The deeper fix —
    // binding the IPC listener BEFORE the heavy handle setup so readiness
    // is near-instant regardless — is tracked as the #1210 follow-up.)
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if client
            .ping_with_timeout(Duration::from_millis(250))
            .await
            .is_ok()
        {
            // 4. Card 282850c2: announce so a sandboxed agent
            // attaching later finds this daemon instead of orphaning
            // a new one. Best-effort — if the announcement fails,
            // the normal singleton-per-home model still works.
            announce_running_daemon(home, &socket, project_root.as_deref(), daemon_pid).await;
            return Ok(socket);
        }
        if Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(format!("daemon did not become ready; see {}", log.display()).into())
}

/// Read `peer_id`/`build` from the freshly-ready daemon and write a
/// discovery entry. Called from `ensure_daemon_running` after the
/// readiness ping succeeds; failure is silent.
async fn announce_running_daemon(
    home: &Path,
    socket: &Path,
    project_root: Option<&Path>,
    pid: u32,
) {
    let client = DaemonClient::new(socket.to_path_buf());
    let Ok(status) = client.status_with_timeout(Duration::from_millis(250)).await else {
        return;
    };
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let entry = crate::discovery::DiscoveredDaemon {
        socket: socket.to_path_buf(),
        home: home.to_path_buf(),
        project_root: project_root.map(Path::to_path_buf),
        peer_id: status.peer_id,
        pid,
        started_at_ms: now_ms,
        build: status.build_commit.unwrap_or_else(|| "unknown".to_string()),
    };
    let _ = crate::discovery::announce(&entry);
}

fn daemon_status_is_current(status: &airc_ipc::StatusResponse) -> bool {
    status.ipc_protocol_version == Some(u32::from(airc_ipc::IPC_PROTOCOL_VERSION))
        && crate::build_info::is_unknown_or_matches(status.build_commit.as_deref())
}

async fn wait_for_daemon_exit(client: &DaemonClient, max_wait: Duration) {
    let deadline = Instant::now() + max_wait;
    while Instant::now() < deadline {
        if client
            .ping_with_timeout(Duration::from_millis(150))
            .await
            .is_err()
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Hand the spawned daemon a `GH_TOKEN` so its account-registry loop can
/// authenticate to the gh-gist rendezvous.
///
/// The daemon talks to GitHub via `gh`, but `gh auth login` stores its
/// OAuth token in the OS keyring (Windows Credential Manager / macOS
/// Keychain), and a `DETACHED_PROCESS` daemon can't always reach that
/// keyring from its spawned session — so its `gh auth status` gate
/// returns "not authenticated" and the loop never publishes. THIS is the
/// real same-account cross-machine blocker: the daemon is the only place
/// holding the live LAN endpoint, so when it can't publish, beacons go
/// out endpoint-less (via the manual `registry sync` fallback) and peers
/// enrol each other but never route.
///
/// The parent here runs in the user's interactive session and DOES have
/// working auth, so we resolve the token it would use (`gh auth token`)
/// and pass it down as `GH_TOKEN` — env-based auth that works in any
/// process context, keyring or not. Derived from the live credential at
/// spawn time (no hardcoding, no a-priori knowledge — same-account =
/// same grid, automatically). Best-effort: if `GH_TOKEN`/`GITHUB_TOKEN`
/// is already set we inherit it untouched; if the parent isn't authed
/// either, we set nothing and the daemon degrades exactly as before
/// (skips the optional rendezvous cleanly).
fn inject_gh_token(command: &mut Command) {
    // Hermetic-isolation opt-out. Integration tests (and any caller
    // that deliberately runs against a throwaway `$HOME`) set
    // `AIRC_NO_GH_TOKEN_INJECT=1` so a daemon spawned under that clean
    // room does NOT reach out to the host's real `gh` credential for
    // the OPTIONAL account rendezvous. Without this, on a gh-authed
    // host the daemon is handed the real machine token but points at a
    // mismatched throwaway home, the rendezvous fails, and the
    // foreground command that spawned it inherits the failure. The
    // rendezvous is best-effort; it must never be the reason a clean
    // CLI invocation exits non-zero.
    if std::env::var("AIRC_NO_GH_TOKEN_INJECT")
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
    {
        return;
    }
    // Only inherit an existing token if it is NON-EMPTY — an
    // exported-but-empty `GITHUB_TOKEN=""` (common in some shells/CI)
    // must NOT short-circuit extraction, or the daemon inherits a blank
    // token and `gh auth status` fails.
    let has_real = |k: &str| {
        std::env::var(k)
            .ok()
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false)
    };
    if has_real("GH_TOKEN") || has_real("GITHUB_TOKEN") {
        return;
    }
    let token = match resolve_gh_token() {
        Some(token) => token,
        None => {
            eprintln!(
                "airc: could not resolve a gh token to hand the daemon — its account-registry \
                 loop will skip the same-account rendezvous (run `gh auth login`, or set GH_TOKEN)"
            );
            return;
        }
    };
    eprintln!(
        "airc: provisioning daemon with GH_TOKEN (len {}) for the account rendezvous",
        token.len()
    );
    command.env("GH_TOKEN", token);
}

/// The operator's `AIRC_GH_BIN` override, if set non-empty (card
/// 1f2cbffa, #1145 audit item 3). When present it is AUTHORITATIVE for
/// every gh resolution in this process — token extraction, the
/// daemon's gate + store, the manual `registry sync` gate.
pub(crate) fn gh_bin_override() -> Option<std::path::PathBuf> {
    let raw = std::env::var_os("AIRC_GH_BIN")?;
    if raw.is_empty() {
        return None;
    }
    Some(std::path::PathBuf::from(raw))
}

/// Known gh install locations, tried after bare `gh` on PATH. Only
/// consulted when the operator has NOT set `AIRC_GH_BIN`.
fn default_gh_candidates() -> Vec<std::path::PathBuf> {
    let mut candidates: Vec<std::path::PathBuf> = vec![std::path::PathBuf::from("gh")];
    #[cfg(windows)]
    {
        candidates.push(std::path::PathBuf::from(
            r"C:\Program Files\GitHub CLI\gh.exe",
        ));
        candidates.push(std::path::PathBuf::from(
            r"C:\Program Files (x86)\GitHub CLI\gh.exe",
        ));
    }
    #[cfg(not(windows))]
    {
        candidates.push(std::path::PathBuf::from("/opt/homebrew/bin/gh"));
        candidates.push(std::path::PathBuf::from("/usr/local/bin/gh"));
        candidates.push(std::path::PathBuf::from("/usr/bin/gh"));
    }
    candidates
}

/// `gh auth token` from the parent, robust to PATH-resolution quirks in
/// a bash-descended process (where bare `gh` may not resolve the same as
/// in an interactive shell). Honors `AIRC_GH_BIN` (the override is the
/// ONLY candidate — a broken override must fail loudly upstream, never
/// silently swap to a different gh); otherwise tries `gh` on PATH, then
/// known install locations.
fn resolve_gh_token() -> Option<String> {
    resolve_gh_token_with(gh_bin_override())
}

/// Env-free body of [`resolve_gh_token`] — `override_bin` is the
/// operator's `AIRC_GH_BIN`, parameterized so tests can pin the
/// override contract without racy process-env mutation.
fn resolve_gh_token_with(override_bin: Option<std::path::PathBuf>) -> Option<String> {
    let candidates = match override_bin {
        Some(bin) => vec![bin],
        None => default_gh_candidates(),
    };
    for bin in candidates {
        let Ok(output) = std::process::Command::new(&bin)
            .args(["auth", "token"])
            .output()
        else {
            continue;
        };
        if !output.status.success() {
            continue;
        }
        let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !token.is_empty() {
            return Some(token);
        }
    }
    None
}

/// The gh executable the daemon should use. `AIRC_GH_BIN` wins
/// unconditionally when set (#1145 audit item 3: this used to be
/// ignored here, and `run_daemon` then clobbered the store's
/// env-honoring default via `.with_bin(...)`). Otherwise: the first
/// candidate that actually responds (`gh --version` exits zero),
/// trying PATH then known install locations. The daemon hands this
/// explicit path to its account-registry gate + store so a
/// bash-format / install-dir-less PATH can't make `Command::new("gh")`
/// silently fail. Returns `None` if gh isn't installed — the daemon then
/// degrades to bare `gh` (and the rendezvous skips cleanly if that too
/// can't resolve).
fn resolve_gh_bin() -> Option<std::path::PathBuf> {
    resolve_gh_bin_with(gh_bin_override())
}

/// Env-free body of [`resolve_gh_bin`] — `override_bin` is the
/// operator's `AIRC_GH_BIN`, parameterized so tests can pin the
/// override contract without racy process-env mutation.
///
/// The override is returned EVEN IF nothing exists at that path —
/// with a loud stderr line, never a silent fallback to a PATH-resolved
/// gh: an operator override that quietly stops applying is the worst
/// failure shape (every subsequent gh spawn then fails loudly per
/// tick, which is diagnosable; a wrong-but-working gh is not).
fn resolve_gh_bin_with(override_bin: Option<std::path::PathBuf>) -> Option<std::path::PathBuf> {
    if let Some(bin) = override_bin {
        if !bin.exists() {
            eprintln!(
                "airc: AIRC_GH_BIN is set to {} but no file exists there — honoring the \
                 override anyway (no silent fallback to a PATH-resolved gh); gh calls will \
                 fail loudly until the path is fixed or the override unset",
                bin.display()
            );
        }
        return Some(bin);
    }
    for bin in default_gh_candidates() {
        if let Ok(output) = std::process::Command::new(&bin).arg("--version").output() {
            if output.status.success() {
                return Some(bin);
            }
        }
    }
    None
}

#[cfg(unix)]
fn detach_daemon(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: this closure runs in the child just before exec and
    // only calls setsid, which is async-signal-safe.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(windows)]
fn detach_daemon(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x00000008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);

    // Stop the detached daemon from inheriting THIS process's standard
    // handles. When `airc` is itself launched with piped stdio — every
    // CLI integration test does (`Command::output()`), and so do agent
    // harnesses that capture our output — our stdout/stderr are the
    // inheritable write-ends of a pipe the parent reads to EOF. Rust
    // spawns children with `bInheritHandles=TRUE` (it must, to hand the
    // daemon its redirected log-file handle), and std offers no way to
    // scope WHICH handles inherit, so without intervention the daemon
    // also inherits the parent's pipe write-end and keeps it open for
    // its whole life. The launching `airc init`/`send` then exits, but
    // the parent's read never sees EOF (the daemon still holds a writer)
    // and `.output()` blocks forever. That is the owner-core lifecycle
    // hang on the self-hosted Windows runner (card 8763f167): the
    // daemon outlives its launcher, so any captured-stdout caller
    // deadlocks. Clearing the inherit flag on our own std handles makes
    // the next CreateProcess (this daemon) leave them behind; the
    // daemon's log-file handles are separate and still inherit fine.
    //
    // SAFETY: `clear_std_handle_inheritance` only calls `GetStdHandle` +
    // `SetHandleInformation` (kernel32) on this process's own standard
    // handles, with valid in-range arguments and the result ignored —
    // no raw pointers, no aliasing, no lifetime concerns. It is sound to
    // call at any point; worst case a handle we can't touch is left as-is.
    unsafe {
        clear_std_handle_inheritance();
    }
}

/// Clear `HANDLE_FLAG_INHERIT` on this process's std handles so a
/// subsequently-spawned child does not duplicate them. `GetStdHandle` /
/// `SetHandleInformation` live in kernel32, which is always linked — no
/// crate dependency. Best-effort: a handle we can't touch is left as-is
/// (the daemon redirects its own stdio regardless).
#[cfg(windows)]
unsafe fn clear_std_handle_inheritance() {
    extern "system" {
        fn GetStdHandle(n_std_handle: u32) -> isize;
        fn SetHandleInformation(h_object: isize, dw_mask: u32, dw_flags: u32) -> i32;
    }
    const STD_INPUT_HANDLE: u32 = -10i32 as u32;
    const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
    const STD_ERROR_HANDLE: u32 = -12i32 as u32;
    const HANDLE_FLAG_INHERIT: u32 = 0x0000_0001;
    const INVALID_HANDLE_VALUE: isize = -1;
    for id in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        let handle = GetStdHandle(id);
        if handle != 0 && handle != INVALID_HANDLE_VALUE {
            // Clear only the inherit bit; the handle stays valid for our
            // own use (this process keeps writing to stdout normally).
            let _ = SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0);
        }
    }
}

/// Push the current scope's peer trust into the running daemon's
/// in-memory registry. In the owner-core there is no per-wire subscribe:
/// the one machine daemon already routes every channel through its
/// `EventRouter`, so a scope just needs the daemon to know its peers
/// (for cross-machine verify), nothing more.
async fn sync_daemon_peers_for_current_rooms(
    home: &Path,
    socket: PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = Airc::open(home).await?;
    let client = DaemonClient::new(socket);
    let set = airc.subscription_set().await?;
    sync_daemon_peers(&client, home, &set).await?;
    Ok(())
}

async fn sync_daemon_peers(
    client: &DaemonClient,
    home: &Path,
    set: &airc_lib::SubscriptionSet,
) -> Result<(), Box<dyn std::error::Error>> {
    sync_daemon_peers_from_store(client, home).await?;
    for subscription in set.all() {
        if let Some(wire_root) = subscription
            .as_room()
            .wire
            .parent()
            .and_then(|path| path.parent())
        {
            if wire_root != home {
                sync_daemon_peers_from_store(client, wire_root).await?;
            }
        }
    }
    Ok(())
}

async fn sync_daemon_peers_from_store(
    client: &DaemonClient,
    home: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    for peer in peers_store::load(home).await? {
        client
            .add_peer(AddPeerRequest {
                peer_id: peer.peer_id,
                pubkey_b64: peer.pubkey_b64,
            })
            .await?;
    }
    Ok(())
}

/// Tell the operator which scope they actually joined and whether
/// it's sharing the machine-account wire or running isolated. The
/// substrate already routes correctly; this is purely diagnostic so
/// `airc join` from a project dir doesn't leave anyone wondering
/// "am I on the same mesh as my HOME tabs?"
///
/// Codex's criterion #2: "airc join from a project scope must
/// converge onto the same usable account mesh or clearly show which
/// scope it is using."
fn print_scope_context(home: &Path, wire: &Path) {
    // wire = <wire_root>/wires/<channel> → wire_root is two parents up.
    let wire_root = wire
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf());
    let scope = home.canonicalize().unwrap_or_else(|_| home.to_path_buf());
    let wire_root_canon = wire_root
        .as_ref()
        .map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()));
    let canonical_account_home = canonical_machine_account_home();
    println!("scope:   {}", scope.display());
    match (&wire_root_canon, &canonical_account_home) {
        // Scope IS the canonical $HOME/.airc machine-account home.
        // It IS the wire root by definition — but that's the
        // intended "everybody on this machine routes here" home,
        // NOT a "project-local isolated" scope. Label accordingly.
        (Some(root), Some(account_home)) if root == &scope && &scope == account_home => {
            println!(
                "mesh:    machine-account home (this is the canonical `{}` — all scopes on this user's machine route here)",
                scope.display()
            );
        }
        // Scope is its own wire root AND not the canonical machine-
        // account home — genuinely isolated (tempdirs, CI harnesses,
        // explicit AIRC_HOME=/tmp/... overrides).
        (Some(root), _) if root == &scope => {
            println!(
                "mesh:    project-local (this scope's identity AND wire live in `{}` — sends are isolated to this dir)",
                scope.display()
            );
        }
        // Scope is a subdir under $HOME but the wire is promoted up
        // to $HOME/.airc — the common "agent ran airc join from a
        // project" case.
        (Some(root), _) => {
            println!(
                "mesh:    machine-account (this scope shares wire at `{}` with every other scope on this user's machine)",
                root.display()
            );
        }
        (None, _) => {
            println!("mesh:    unknown (could not resolve wire root)");
        }
    }
}

fn canonical_machine_account_home() -> Option<PathBuf> {
    let user_home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)?;
    let user_home_canon = user_home.canonicalize().unwrap_or(user_home);
    Some(user_home_canon.join(".airc"))
}

/// Honest one-line receipt for a completed `send`/`msg`/`publish`.
///
/// What is TRUE synchronously, by the time the publish call returns:
/// the frame is signed, persisted to the local store, fanned out to
/// in-process subscribers, written to the wire, and offered to the
/// routed-forward tap. Any scope tailing this channel on THIS machine
/// has it.
///
/// What is NOT knowable here: whether any remote peer actually
/// received it. `peer_count` is the count of cryptographically-paired
/// *enrolled* remote peers ([`Airc::peers`]) — an address book, not a
/// delivery receipt. Routed forwarding and its delivery acks are
/// drained by [`airc_lib::RoutedForwarder`] in a background loop that
/// has not run by the time the CLI returns, so there is no honest
/// synchronous forwarded/acked count to print.
///
/// Therefore the verb is "queued"/"addressed", never "sent to N
/// peers" — the latter implied confirmed delivery to the enrolled
/// count and produced false "it works" claims when zero peers had
/// actually received anything. To confirm delivery, the operator runs
/// `airc doctor --health` (route status), which reads the forwarder's
/// real ack counters.
///
/// `connected_lan_peers` (from `Status`, refreshed by the daemon's
/// route-refresh loop) is the set room broadcast can ACTUALLY reach
/// right now — the forwarder fans out only over live LAN connections.
/// When it is `0` while peers are enrolled, the send reached NO remote
/// machine, and the receipt says so LOUDLY: enrolling a peer is an
/// address-book entry, not a live route, and a silently-zero connected
/// set is exactly how a fully-broken fan-out masqueraded as a healthy
/// channel (issue #1243 — the loopback-only legibility trap).
fn format_send_receipt(
    channel_name: &str,
    channel_id: &str,
    enrolled_peers: usize,
    connected_lan_peers: usize,
) -> String {
    if enrolled_peers == 0 {
        format!(
            "queued to {channel_name} ({channel_id}) — 0 enrolled remote peer(s); \
             any scope tailing this channel on this machine will receive it."
        )
    } else if connected_lan_peers == 0 {
        // LOUD: enrolled peers exist but the daemon holds no live LAN
        // connection, so room broadcast forwarded to nobody. This is the
        // signal whose absence let a broken fan-out read as success.
        format!(
            "queued to {channel_name} ({channel_id}) — ⚠ reached 0 of {enrolled_peers} \
             enrolled remote peer(s): NONE are currently connected, so no remote machine \
             received this (run `airc doctor --health` to see why the routes are down). \
             Any scope tailing this channel on this machine still has it."
        )
    } else {
        // Report REACH, not the address book. `enrolled_peers` is every peer this
        // scope has ever enrolled — across every room, for all time — so printing
        // it beside a live count invites the reader to divide, and that ratio is
        // meaningless: the enrolled set is not this room's audience, and
        // `connected_lan_peers` counts LAN links only.
        //
        // Live 2026-08-07 that arithmetic manufactured a false alarm. "addressed 52
        // enrolled remote peer(s), 1 currently connected" reads as 2% reach; the
        // ack ledger for the same period showed 767 of 771 delivered — 99.5%. A
        // card was filed on the strength of the receipt (#340) and the receipt was
        // the only thing wrong. An instrument that reads as a catastrophe during
        // healthy operation is worse than no instrument: it burns the operator's
        // trust in every future alarm.
        //
        // So: state the live count as a plain fact with no denominator to divide
        // by, and point at `airc doctor --health`, which reads the ACK ledger
        // (#280 — delivery is a returned ack, never a connection's existence).
        let peers = if connected_lan_peers == 1 {
            "1 peer"
        } else {
            "peers"
        };
        let count = if connected_lan_peers == 1 {
            String::new()
        } else {
            format!("{connected_lan_peers} ")
        };
        format!(
            "queued to {channel_name} ({channel_id}) — live to {count}{peers} now; \
             delivery is asynchronous and confirmed by ACK, not by this line \
             (run `airc doctor --health` for the delivery ledger). \
             {enrolled_peers} peer(s) are enrolled in this scope's address book, \
             which is not this room's audience — do not read it as a reach ratio. \
             Any scope tailing this channel on this machine also receives it."
        )
    }
}

/// Presence horizon for the @mention audience check. Generous on
/// purpose: a peer who reads this room but is between heartbeats must
/// still count as audience — this warning exists to catch a peer who
/// has NEVER been seen here, not to police liveness.
const MENTION_AUDIENCE_WITHIN: Duration = Duration::from_secs(48 * 60 * 60);
/// Presence-scan window for the @mention audience check (events).
const MENTION_AUDIENCE_WINDOW: usize = 4096;

/// The leading `@name` of a message body — the human addressing
/// convention (`airc msg @peer …`). The `@` is a label, not routing:
/// delivery is still room broadcast, which is exactly why the audience
/// check below exists. Returns the bare name without `@`.
fn leading_mention(text: &str) -> Option<&str> {
    let rest = text.strip_prefix('@')?;
    let end = rest
        .find(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_'))
        .unwrap_or(rest.len());
    (end > 0).then(|| &rest[..end])
}

/// #270 family, live-proven 2026-08-12: a message addressed `@peer` was
/// sent — twice, with green receipts — into a room whose channel id was
/// the sender's own operator uuid, a room the addressed peer cannot
/// hear. The receipt never said so, and the miss was diagnosed as a
/// transport failure for an hour. This check resolves the leading
/// @mention against the room's roster (durable presence + identity
/// join) and returns a LOUD warning line when nobody matching the
/// mention has ever been seen in the room. Advisory only: the send has
/// already happened, the roster is presence-derived (an offline-but-
/// subscribed peer beyond the horizon is a possible false alarm — the
/// wording says "not seen", never "not subscribed"), and any roster
/// error yields `None` so the check can never break a send.
async fn mention_audience_warning(
    airc: &Airc,
    text: &str,
    channel: RoomId,
    channel_name: &str,
) -> Option<String> {
    let mention = leading_mention(text)?;
    let roster = airc
        .room_roster_in(
            Some(channel),
            MENTION_AUDIENCE_WITHIN,
            MENTION_AUDIENCE_WINDOW,
        )
        .await
        .ok()?;
    mention_audience_verdict(mention, &roster, airc.peer_id(), channel_name)
}

/// The decision half of [`mention_audience_warning`], split out from the
/// roster fetch so the wording is testable without an `Airc`.
///
/// The strong "has not been seen" line asserts ABSENCE. That is only
/// honest when the roster could have answered the question at all — every
/// OTHER peer in the window is named, so a failed match really does mean
/// "not seen in this window". (Never "not subscribed": a peer quieter than
/// [`MENTION_AUDIENCE_WITHIN`] is absent from the roster entirely, which is
/// why the emitted string says "not seen" and hedges the rest.)
/// `RoomMember::display_name` is documented as `None` for a peer that is
/// PRESENT but has not published an identity card; a mention that matches
/// nobody may simply BE one of those peers.
///
/// #262 caught the all-unnamed case. #1378 caught the same defect one
/// level up, live on IntelMac 2026-09-04: both other grid peers were
/// reported as "will NEVER receive this" seconds after each had posted,
/// because a single OTHER named member satisfied `any()`. One named member
/// cannot license a negative about a different, unnamed one — so the guard
/// is `all`, not `any`.
///
/// Two things the #1378 review then caught in that fix (card 74e8e6af):
///
/// - **`me` is excluded from the accounting.** A roster includes `self`, so
///   an operator who has never run `airc identity set` is an unnamed member
///   of EVERY room: `unnamed > 0` would be permanently true, the strong line
///   could never fire, and the soft line would describe the operator to
///   themselves as a third party. `heard` still scans the FULL roster, so
///   mentioning your own name resolves rather than warning.
/// - **Peers are counted distinctly.** `room_roster_in` yields one row per
///   (peer, client session) — two agent tabs on one box are two rows sharing
///   a peer id — so a row count printed as "peer(s)" is a number the data
///   does not support.
fn mention_audience_verdict(
    mention: &str,
    roster: &[airc_lib::RoomMember],
    me: airc_lib::PeerId,
    channel_name: &str,
) -> Option<String> {
    let needle = mention.to_ascii_lowercase();
    let heard = roster.iter().any(|member| {
        member
            .display_name
            .as_deref()
            .is_some_and(|name| name.eq_ignore_ascii_case(mention))
            || member
                .peer_id
                .to_string()
                .to_ascii_lowercase()
                .starts_with(&needle)
    });
    if heard {
        return None;
    }
    // Both clauses matter and neither depends on anyone's naming state: the
    // "send in a room they read" half is what actually ended #270's hour of
    // misdiagnosis, and after the `all` guard the soft path is the COMMON
    // path in any room holding an uncarded peer.
    let hint = "Check where they speak (`airc events list --limit 5000 --kind message`) \
                and send in a room they read, e.g. `airc msg --room general ...`.";
    let others: Vec<&airc_lib::RoomMember> = roster.iter().filter(|m| m.peer_id != me).collect();
    let distinct = |members: &[&airc_lib::RoomMember]| {
        members
            .iter()
            .map(|m| m.peer_id)
            .collect::<std::collections::HashSet<_>>()
            .len()
    };
    if others.is_empty() {
        return Some(format!(
            "⚠ cannot verify '@{mention}' reaches '{channel_name}': no peer other than \
             yourself has been seen in this room within the presence window, so this \
             mention cannot be resolved either way (#262). {hint}"
        ));
    }
    let unnamed: Vec<&airc_lib::RoomMember> = others
        .iter()
        .copied()
        .filter(|m| m.display_name.is_none())
        .collect();
    if !unnamed.is_empty() {
        return Some(format!(
            "⚠ cannot verify '@{mention}' reaches '{channel_name}': {} of {} peer(s) seen \
             here have not published an identity card, so a name match cannot succeed \
             against them either way (#262). {hint}",
            distinct(&unnamed),
            distinct(&others)
        ));
    }
    Some(format!(
        "⚠ '@{mention}' has not been seen in '{channel_name}' (48h presence window) — \
         if they are not subscribed to this room they will NEVER receive this. {hint}"
    ))
}

/// `send` — local-fs single-shot send to the current room. Routes
/// through `Airc::say`; ad-hoc `--peer` flags are enrolled in the
/// in-process registry for the duration of the invocation.
/// Open an `Airc` attached to this machine's singular daemon, starting
/// it if needed. Same-machine send/read/subscribe route through the
/// daemon's router — the only same-machine path (no more `frames.jsonl`).
pub(crate) async fn attached_airc(home: &Path) -> Result<Airc, Box<dyn std::error::Error>> {
    let socket = crate::cli::default_socket_path_in(home);
    let socket = ensure_daemon_running(home, socket, Vec::new()).await?;
    Ok(Airc::attach(home, socket).await?)
}

pub async fn run_send(
    home: &Path,
    peers: Vec<PeerSpec>,
    room: Option<&str>,
    text: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = attached_airc(home).await?;
    for peer in &peers {
        airc.enrol_volatile_peer(peer)?;
    }
    // Card a979e5c2 reviewer NIT — symmetric with run_msg: keep the
    // daemon's per-wire subscriber index aware of every subscribed
    // room before the send goes out. Load-bearing in particular when
    // `--room <name>` targets a room the daemon hasn't recently
    // touched. Safe no-op when the index is already fresh.
    let socket = crate::cli::default_socket_path_in(home);
    sync_daemon_peers_for_current_rooms(home, socket).await?;
    // Card a979e5c2 (seam #5): `--room <name>` routes ONE message
    // to a subscribed-but-not-current room without mutating this
    // scope's default-room pointer. Same shape as `airc publish`.
    // Without `--room`, the historical "current room + runtime
    // headers" path runs unchanged.
    let (channel_name, channel) = match room {
        Some(name) => {
            let receipt = airc
                .publish(
                    airc_lib::PublishTarget::RoomByName(name.to_string()),
                    airc_protocol::FrameKind::Message,
                    airc_core::Body::text(text),
                    runtime_headers()?,
                )
                .await?;
            (receipt.channel_name, receipt.channel_id)
        }
        None => {
            let current = airc.current_room().await?;
            airc.say_with_headers(text, runtime_headers()?).await?;
            (current.name, current.channel)
        }
    };
    let channel_id = channel.to_string();
    // `peers()` is the enrolled-remote-peer address book, NOT a
    // delivery count — see `format_send_receipt` for why the receipt
    // says "queued/addressed" rather than "sent to N peers".
    let peer_count = airc.peers().await?.len();
    // Ask the daemon how many of those peers it currently holds a LIVE
    // LAN connection to — the set room broadcast can actually reach.
    // A cheap Status round-trip; on any failure we fall back to 0,
    // which the receipt frames as "none currently connected" rather
    // than inventing reach the daemon can't confirm.
    let connected_lan_peers = DaemonClient::new(crate::cli::default_socket_path_in(home))
        .status()
        .await
        .map(|status| status.connected_lan_peers)
        .unwrap_or(0);
    println!(
        "{}",
        format_send_receipt(&channel_name, &channel_id, peer_count, connected_lan_peers)
    );
    if let Some(warning) = mention_audience_warning(&airc, text, channel, &channel_name).await {
        println!("{warning}");
    }
    Ok(())
}

/// `listen` — subscribe to live events on the current room and
/// print them until Ctrl-C. Routes through `Airc::subscribe`. The
/// underlying wire subscriber is replay-anchored: existing frames
/// on the wire are replayed through the broadcast first, then live
/// events flow. `--replay` is accepted for compatibility but the
/// distinction is no longer load-bearing — the substrate always
/// gives consumers the full transcript.
pub async fn run_listen(
    home: &Path,
    peers: Vec<PeerSpec>,
    _replay: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = attached_airc(home).await?;
    for peer in &peers {
        airc.enrol_volatile_peer(peer)?;
    }
    let current = airc.current_room().await?;
    println!(
        "listening on {} ({}, peer_id {}) …",
        current.name,
        current.wire.display(),
        airc.peer_id()
    );

    // Subscribe creates the live receiver BEFORE spawning the wire
    // subscriber (see `Airc::subscribe`), so pre-existing frames
    // on the wire flow through this stream without race-loss.
    let mut stream = airc.subscribe().await?;
    print_event_stream_until_signal(&mut stream).await
}

/// `lan-send` — TLS-wrapped single-shot send to a remote peer, on
/// the current room's channel, with a delivery-ack wait.
///
/// Card 39d37629: "sent" used to mean "bytes flushed to the TLS
/// socket" — a receiver could accept the frame and silently lose it
/// before transcript persistence (live repro 2026-06-12 02:36Z: 5090
/// → mac printed "sent over lan-tcp to general", exit 0; the frame
/// reached NO store on the receiving machine). The verb now requests
/// a typed ack that the receiver emits only AFTER its persistence
/// decision, waits up to `ack_timeout_ms`, prints the typed outcome,
/// and exits nonzero for anything that is not `delivered`.
pub async fn run_lan_send(
    home: &Path,
    peers: Vec<PeerSpec>,
    to: std::net::SocketAddr,
    expected_peer: PeerId,
    ack_timeout_ms: u64,
    text: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    // Card bf7c30e2: fail FAST with a self-diagnosing error before
    // dialing. Trust stores are per-scope (cwd's git root), so a peer
    // enrolled in one directory is invisible from another; without
    // this preflight the mismatch surfaced as a mid-TLS-handshake
    // "cert pubkey is not enrolled" that named neither the store it
    // consulted nor the likely cause — which cost a live cross-machine
    // route outage and a three-message debugging exchange. The walker
    // only knows its cwd; the error must tell it which world it's in.
    //
    // Review round 1 caught the first version checking ONLY the scope
    // store while the TLS verifier pins against the FULL union
    // (scope + machine-account store + wire-root imports via
    // `load_peer_registries`) — which would have refused dials that
    // work today (same-machine loopback, account-imported peers).
    // The preflight now asks the opened handle's `peers()`, which is
    // that exact union: preflight sources == verifier sources, by
    // construction.
    let airc = Airc::open(home).await?;
    for peer in &peers {
        airc.enrol_volatile_peer(peer)?;
    }
    preflight_expected_peer(&airc, home, &peers, expected_peer).await?;
    let current = airc.current_room().await?;
    airc.connect_lan(to, expected_peer).await?;
    let timeout = std::time::Duration::from_millis(ack_timeout_ms);
    let outcome = airc
        .send_with_delivery_ack(text, runtime_headers()?, timeout)
        .await?;
    match outcome {
        airc_lib::DeliverySendOutcome::Delivered { event_id, ack } => {
            match ack.outcome {
                airc_lib::DeliveryOutcome::Delivered { channel, cursor } => {
                    println!(
                        "delivered over lan-tcp to {} ({channel}) — receiver {} persisted \
                         event {event_id} at lamport {}.",
                        current.name, ack.receiver, cursor.lamport
                    );
                }
                // `DeliverySendOutcome::Delivered` is constructed only
                // from a Delivered ack; keep the match honest anyway.
                airc_lib::DeliveryOutcome::Undeliverable { reason } => {
                    return Err(format!(
                        "internal: delivered outcome carried undeliverable ack ({})",
                        reason.as_str()
                    )
                    .into());
                }
            }
            Ok(())
        }
        airc_lib::DeliverySendOutcome::Undeliverable { event_id, ack } => {
            let reason = match ack.outcome {
                airc_lib::DeliveryOutcome::Undeliverable { reason } => reason.as_str(),
                airc_lib::DeliveryOutcome::Delivered { .. } => "unknown",
            };
            Err(format!(
                "NOT delivered: receiver {} reports event {event_id} undeliverable \
                 (reason: {reason}). The frame was accepted on the wire but will not \
                 appear in the receiving scope's {} transcript.",
                ack.receiver, current.name
            )
            .into())
        }
        airc_lib::DeliverySendOutcome::NoAck { event_id, waited } => Err(format!(
            "NOT confirmed: no delivery ack for event {event_id} within {}ms. The frame \
             was flushed to the wire, but the receiver never confirmed persistence — it \
             may be running an older build (which never acks) or it dropped the frame. \
             Treat as undelivered.",
            waited.as_millis()
        )
        .into()),
    }
}

/// Self-healing join — `airc dial HOST:PORT`: manual recovery dial with
/// the full authenticated handshake, LOUD either way.
///
/// Why it exists (M5↔bigmama live repro): when the automatic paths are
/// wedged — a peer dialing our dead port, a poisoned record — ONE
/// hands-on authenticated dial both proves reachability AND teaches the
/// REMOTE side our real source address (learn-live-address, #9), which
/// un-wedges its next outbound dial. This is the manual override that
/// used to require hand-driven `lan-send` gymnastics.
///
/// The expected peer (for mTLS cert pinning) is inferred when `--peer`
/// is omitted: first a stored-endpoint exact match, then the
/// identity-derived stable port (#8). Ambiguity or no match is a loud
/// error naming the candidates — never a guess.
pub async fn run_dial(
    home: &Path,
    peers: Vec<PeerSpec>,
    to: std::net::SocketAddr,
    expected_peer: Option<PeerId>,
    timeout_ms: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = Airc::open(home).await?;
    for peer in &peers {
        airc.enrol_volatile_peer(peer)?;
    }

    let expected = match expected_peer {
        Some(peer) => peer,
        None => infer_peer_for_endpoint(&airc, home, to).await?,
    };
    preflight_expected_peer(&airc, home, &peers, expected).await?;

    println!("dialing {to} expecting peer {expected} …");
    let deadline = std::time::Duration::from_millis(timeout_ms);
    match tokio::time::timeout(deadline, airc.connect_lan(to, expected)).await {
        Ok(Ok(())) => {
            // A successful authenticated dial IS fresh contact — stamp
            // recency so the liveness/ghost classifiers see it (same
            // contract as the registry import's touch). Best-effort on
            // both stores; `Ok(None)` just means the peer lives in the
            // other one.
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            let _ = airc_trust::touch_last_seen(airc.wire_root(), expected, now_ms).await;
            let _ = airc_trust::touch_last_seen(home, expected, now_ms).await;
            println!(
                "CONNECTED: authenticated handshake with {expected} at {to} succeeded.\n\
                 The remote has now learned THIS machine's real source address \
                 (learn-live-address) — its next outbound dial can use it even if its \
                 stored endpoint for us is stale."
            );
            Ok(())
        }
        Ok(Err(error)) => Err(format!(
            "DIAL FAILED: {to} (expecting {expected}) — {error}\n\
             The endpoint answered nothing acceptable. Check `airc network` for the \
             peer's freshest advertisement, or `airc registry sync` to re-read the \
             rendezvous."
        )
        .into()),
        Err(_elapsed) => Err(format!(
            "DIAL FAILED: {to} (expecting {expected}) — no TCP+TLS handshake within \
             {timeout_ms}ms (endpoint unreachable, or a firewall drops SYN)."
        )
        .into()),
    }
}

/// Infer which enrolled peer owns `to` for cert pinning: an exact match
/// against stored endpoints first, else the identity-derived stable
/// port (#8 — `stable_lan_port(peer_id) == to.port()`). Exactly one
/// candidate wins; zero or several is a loud error asking for `--peer`.
async fn infer_peer_for_endpoint(
    airc: &Airc,
    home: &Path,
    to: std::net::SocketAddr,
) -> Result<PeerId, Box<dyn std::error::Error>> {
    let mut stored = airc_trust::load(airc.wire_root()).await.unwrap_or_default();
    if airc.wire_root() != home {
        stored.extend(airc_trust::load(home).await.unwrap_or_default());
    }
    let mut endpoint_matches: Vec<PeerId> = Vec::new();
    let mut port_matches: Vec<PeerId> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for peer in stored {
        if peer.peer_id == airc.peer_id() || !seen.insert(peer.peer_id) {
            continue;
        }
        if let Some(json) = peer.endpoints_json.as_deref() {
            if let Ok(endpoints) = airc_lib::endpoints_from_json(json) {
                let hit = endpoints.iter().any(|endpoint| {
                    matches!(
                        endpoint,
                        airc_lib::RouteEndpoint::LanTcp { addr }
                        | airc_lib::RouteEndpoint::TailscaleTcp { addr } if *addr == to
                    )
                });
                if hit {
                    endpoint_matches.push(peer.peer_id);
                    continue;
                }
            }
        }
        if airc_lib::stable_lan_port(peer.peer_id) == to.port() {
            port_matches.push(peer.peer_id);
        }
    }
    let candidates = if endpoint_matches.is_empty() {
        port_matches
    } else {
        endpoint_matches
    };
    match candidates.as_slice() {
        [one] => {
            println!("inferred expected peer {one} for {to} (trust-store match)");
            Ok(*one)
        }
        [] => Err(format!(
            "cannot infer which peer to expect at {to}: no enrolled peer's stored \
             endpoint or stable port matches. Pass --expected-peer <uuid> (see `airc peers`)."
        )
        .into()),
        several => Err(format!(
            "ambiguous: {} enrolled peers match {to} ({}). Pass --expected-peer <uuid>.",
            several.len(),
            several
                .iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )
        .into()),
    }
}

/// Card bf7c30e2: verify `expected_peer` is known to the SAME trust
/// material the TLS verifier will pin against — the opened handle's
/// `peers()` union (scope store + machine-account store + wire-root
/// imports) plus any ad-hoc `--peer` specs — and if not, say exactly
/// which stores were consulted and what to do about it.
async fn preflight_expected_peer(
    airc: &Airc,
    home: &Path,
    volatile: &[PeerSpec],
    expected_peer: PeerId,
) -> Result<(), Box<dyn std::error::Error>> {
    if volatile.iter().any(|p| p.peer_id == expected_peer) {
        return Ok(());
    }
    // Self-dial is always legal: the verifier registry enrols this
    // scope's own identity (loopback testing), but `peers()` filters
    // self out — without this check the preflight refuses a dial TLS
    // would accept (round-3 review catch).
    if expected_peer == airc.peer_id() {
        return Ok(());
    }
    let enrolled = airc.peers().await?;
    if enrolled.iter().any(|p| p.peer_id == expected_peer) {
        return Ok(());
    }
    Err(format!(
        "peer {expected_peer} is not enrolled in any trust store this command uses:\n  \
         scope store:   {home} \n  \
         machine store: {machine} \n  \
         (union holds {n} peer(s); `airc peers` shows the same view)\n  \
         Trust stores are scoped — the scope comes from the cwd's git root \
         (or $AIRC_HOME). If you enrolled this peer in a different scope, \
         re-run from there, pass --home <that-scope>, or enrol here:\n  \
         airc peer add <uuid>:<pubkey>",
        home = home.display(),
        machine = airc.wire_root().display(),
        n = enrolled.len(),
    )
    .into())
}

/// `lan-listen` — bind a TLS server, accept peers, print frames.
pub async fn run_lan_listen(
    home: &Path,
    peers: Vec<PeerSpec>,
    bind: std::net::SocketAddr,
    _replay: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = Airc::open(home).await?;
    for peer in &peers {
        airc.enrol_volatile_peer(peer)?;
    }
    // Subscribe BEFORE binding the listener. `listen_lan` starts the LAN
    // frame-ingest task, which fans each received frame into `live_tx`
    // (see `append_received_frame`). `subscribe()` is a live broadcast
    // receiver with no backlog for a not-yet-created subscriber, and
    // `lan-listen` does not replay the store — so a frame that arrives in
    // the gap between bind and subscribe is fanned out to no receiver and
    // lost to this consumer (still persisted, just never printed).
    // Creating the receiver first guarantees it predates any ingested
    // frame, closing an intermittent CI frame-drop ("listener did not
    // print the message"). subscribe() does not depend on the listener
    // being bound.
    let mut stream = airc.subscribe().await?;
    let actual = airc.listen_lan(bind).await?;
    println!("listening on {actual} (peer_id {}) …", airc.peer_id());
    print_event_stream_until_signal(&mut stream).await
}

/// `daemon` — run the long-lived daemon process on the given socket.
pub async fn run_daemon(
    home: &Path,
    identity: LocalIdentity,
    peers: Vec<PeerSpec>,
    socket: PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    // REFUSE a socket this home does not own.
    //
    // A daemon serves ONE scope's identity to every client that attaches. If
    // its `--home` is not the home the socket resolves from, it serves the
    // wrong identity to everyone — silently, while looking completely healthy.
    // That is not a degraded mode worth limping in; it is a wrong answer to
    // every question, so it fails at startup instead of running.
    //
    // This is the guard that was missing on 2026-08-12. The spawn passed the
    // caller's project home through to a machine-account socket, and NOTHING
    // objected: daemon up, routes healthy, doctor 10/10 clean, while sends
    // went into a scope the intended peer was not enrolled in. Two operators
    // spent a night diagnosing the symptoms (stale ports, dropped SYNs, a
    // monologue event store, a phantom "inbound not persisting" bug) because
    // the one condition that explained all of them was never checked.
    //
    // The spawn is now correct, so this can only fire on a hand-rolled
    // invocation — which is exactly when a human needs to be told, by name,
    // which two scopes disagree.
    // The invariant that is actually derivable HERE: a daemon's home must
    // BE a machine-account root, not a project scope that merely resolves
    // to one. `machine_account_home` is idempotent on a real machine home,
    // so `machine_account_home(home) != home` is exactly "this is somebody
    // else's scope". The socket path can NOT be re-derived for comparison —
    // `resolve_socket_path` takes the scope home AND the machine home, so
    // the same socket is minted from many scopes by design.
    //
    // EXEMPT a simulated account. `machine_account_home` documents a carve-out:
    // when HOME/USERPROFILE is ITSELF temp-rooted, a harness is simulating a
    // machine account with a TempDir and scopes deliberately SHARE it. Under
    // that simulation `machine_account_home(home) != home` is the NORMAL,
    // correct state, so the check above reads a legitimate pairing as theft.
    //
    // Measured: ~10 `codex_hook_*` tests plus `drain_stdin_timeout` failing with
    // "daemon did not become ready" — a 22s hang each, because the daemon
    // refused to start and the CLI waited out its readiness window. Caught by M5
    // with a positive control (same test passes on canary's parent, fails with
    // the guard) rather than from reading the diff, which is what made it a
    // five-minute fix instead of a hunt.
    //
    // A guard that fires on correct configurations is worse than no guard: it
    // does not merely fail tests, it teaches everyone to route around the check.
    // Enforce only where a foreign socket is genuinely reachable — a real
    // machine account, which is also the only place the original bug can bite.
    let owning_home = airc_lib::machine_account_home(home);
    let simulated_account = airc_core::temp_home::scope_home_is_temp_rooted(home);
    if !simulated_account && owning_home.as_path() != home {
        return Err(format!(
            "refusing to serve a socket this scope does not own.\n  \
             --home  {}\n  \
             --socket {}\n  \
             this home's machine account is {}\n\
             A daemon serves ITS home's identity to every client on that socket, so \
             serving a foreign one silently gives every caller the wrong peer_id, the \
             wrong rooms, and the wrong advertised endpoint. Start the daemon with the \
             home that owns the socket, or let `airc join` spawn it.",
            home.display(),
            socket.display(),
            owning_home.display(),
        )
        .into());
    }
    // Card 800ce5bd: install a tracing subscriber so the existing
    // `tracing::warn!` / `tracing::info!` calls in airc-bus, airc-lib,
    // airc-relay, etc. actually emit. Before this, every tracing call
    // in the workspace was a no-op (no subscriber registered) — load-
    // bearing diagnostics had nowhere to land. `RUST_LOG=info` turns on
    // the fan-out + subscribe instrumentation; default `warn` filter
    // keeps the daemon quiet at steady state. `set_global_default`
    // failures are ignored (a re-run inside the same process shouldn't
    // crash — e.g. in-process tests sharing the daemon entry).
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .finish();
    let _ = tracing::subscriber::set_global_default(subscriber);

    let registry = build_combined_registry(home, &identity, &peers).await?;

    if let Some(parent) = socket.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    // ONE ORM per machine account (§3.3). The daemon is the single
    // owner: every scope under this user's `$HOME` resolves the same
    // machine-account home, so they share one `events.sqlite` — the
    // router's durable transcript + persisted epoch, and the coordinator
    // store's subscriptions / beacons / identity. No per-scope store.
    let machine_home = airc_lib::machine_account_home(home);
    std::fs::create_dir_all(&machine_home)?;
    let db_path = machine_home.join("events.sqlite");
    // Concrete handle kept alongside the trait object: the inbound
    // bridge's unknown-channel heal reads the account-registry CACHE
    // (a concrete SqliteAccountRegistryStore over this same file).
    let machine_store = Arc::new(SqliteEventStore::open_path(&db_path).await?);
    let coordinator_store: Arc<dyn EventStore> = machine_store.clone();
    let state = Arc::new(
        DaemonState::build(
            identity.peer_id,
            identity.keypair,
            registry,
            VerificationPolicy::Strict,
            machine_home.into_path_buf(),
            &db_path,
            coordinator_store,
            current_daemon_runtime_info(),
        )
        .await?,
    );
    println!(
        "airc daemon: peer_id={} listening on {}",
        identity.peer_id,
        socket.display()
    );
    // Card 1998f6cb: the OUTBOUND mirror of the inbound router bridge.
    // Install the routed forwarder on the daemon's router BEFORE any
    // transport handle comes up, so every durable publish (local IPC
    // sends AND bridged inbound frames) is offered to the route layer
    // and traverses established LAN connections — with delivery acks,
    // loop prevention by origin link, and loud bounded-queue drops.
    let routed_forwarder = airc_lib::RoutedForwarder::install(
        &state.router,
        airc_lib::RoutedForwarderConfig::default(),
    );

    // Card 7e3c9a1f: ONE daemon LAN handle, shared across the listener,
    // the dialer (route refresh), and the forwarder. Accepted AND dialed
    // connections then live on the SAME `LanTcpAdapter` connection map, so
    // receive + delivery-ack + routed-forward all see the same peers.
    // Previously the listener handle (registry task) and the route-refresh
    // handle were SEPARATE `Airc::open`s with separate adapters: a
    // connection accepted on the listener was invisible to the dialer
    // handle, so reverse-direction room broadcast — forwarding/acking back
    // over an inbound-accepted connection — failed with "lan-tcp adapter
    // has no connected peers". The inbound sink and the forwarder link are
    // installed ONCE here; the listener bind stays in the registry task
    // (gated), the dial happens in route refresh — all on this one handle.
    let daemon_airc = Airc::open(&state.home).await?;
    daemon_airc.set_inbound_frame_sink(Arc::new(
        airc_lib::RouterInboundBridge::new(state.router.clone(), state.coordinator_store.clone())
            // Self-healing join (the "blind room" heal): an inbound
            // frame for a channel no scope binds consults the local
            // account-registry cache and re-binds from its beacons
            // instead of silently store-and-dropping.
            .with_account_registry(Arc::new(airc_lib::SqliteAccountRegistryStore::new(
                machine_store.clone(),
            ))),
    ));
    routed_forwarder.add_link(daemon_airc.clone()).await;

    // #1306: end-to-end delivery truth. The forwarder's per-peer ack
    // ledger feeds the shared daemon handle so route refresh can (a)
    // force-drop + re-dial "connected" peers whose flushed frames go
    // unacked (the half-open purge) and (b) stamp MEASURED lan-tcp
    // health (rtt/success_ppm) instead of `healthy (not measured)`.
    daemon_airc.set_delivery_ledger(routed_forwarder.delivery_ledger());

    // Self-healing join (refresh-on-failure): the ONE resolved rendezvous
    // store + gate, shared between the registry-refresh loop (its owner,
    // which fills this slot once resolution succeeds) and the
    // route-refresh loop (which re-reads the rendezvous when dials fail,
    // instead of blindly retrying dead endpoints). Empty until the
    // registry task resolves — and stays empty when the rendezvous is
    // disabled (hermetic gate / no store), in which case there is
    // nothing to re-read and the heal path correctly stays off.
    let rendezvous_for_heal: Arc<SharedRendezvousSlot> = Arc::new(std::sync::OnceLock::new());

    // Card 625abe6d slice 2: the daemon, not the operator, keeps
    // routes alive. Spawn the periodic route-discovery refresh before
    // the accept loop blocks; it exits on the same shutdown notifier.
    let route_refresh_task = spawn_route_refresh(
        state.clone(),
        daemon_airc.clone(),
        rendezvous_for_heal.clone(),
    );

    // #240 event-driven heal (peer-DROPPED mirror of the registry-import
    // nudge): when a live LAN session terminates, nudge the route-refresh loop
    // to attempt reconnection AT ONCE — quarantine-gated, so a genuinely-offline
    // peer costs at most one dial, while a transient blip reconnects in seconds
    // instead of waiting out a full refresh interval. Registered on the shared
    // daemon handle; the wake permit coalesces a burst of drops into one refresh.
    {
        let route_wake = state.route_wake.clone();
        daemon_airc.set_disconnect_observer(std::sync::Arc::new(move |_peer_id| {
            route_wake.notify_one();
        }));
    }

    // #1268: autonomous self-update — keep the node on current canary with no
    // human in the loop (the version-drift pain we just lived: stale binaries,
    // peers that can't speak the current protocol). The dangerous half (fetch +
    // smoke-test + rollback-safe `airc update --auto`, spawned detached) lives
    // in `airc_daemon::auto_update`; the daemon supplies only the MESH-IDLE
    // predicate so an update never restarts mid-work.
    //
    // Idle = NO HEALTHY PEER HAS FRAMES OUTSTANDING (`mesh_is_quiet` over the
    // delivery-truth stats, #280). This replaced `connected_lan_peers == 0`,
    // which was inverted: a node connected to the mesh — i.e. one doing its job
    // — was never "idle", so it could never self-update, and only an already-
    // isolated node ever did. That is why two healthy nodes sat on a stale build
    // for a day (2026-08-05) with a merged transport fix available, until a human
    // ran the update by hand on both. Connection COUNT is not work.
    //
    // Note the resolved TODO this carries: a connected-but-quiet node CAN now
    // update, which is the whole point. Exits on the shared shutdown notifier.
    let auto_update_task = {
        let daemon_state = state.clone();
        tokio::spawn(async move {
            airc_daemon::auto_update::run(&daemon_state.shutdown, || {
                // try_read, NOT blocking_read: this predicate is called from
                // inside the async tick, and tokio's RwLock::blocking_read
                // panics in an async context — it would have taken the daemon
                // down on the first tick.
                match daemon_state.delivery_stats.try_read() {
                    Ok(stats) => airc_daemon::auto_update::mesh_is_quiet(
                        stats.iter().map(|s| (s.attempts_since_ack, s.suspect)),
                    ),
                    Err(_) => {
                        // Contended write (stats being refreshed). Skip THIS
                        // tick and retry next interval — never guess "quiet"
                        // from a lock we couldn't read. Loud, because a
                        // permanently-contended lock would silently become a
                        // node that never self-updates again, which is the
                        // exact failure class this whole path exists to kill.
                        eprintln!("airc auto-update: delivery stats busy — skipping this check");
                        false
                    }
                }
            })
            .await;
        })
    };

    // KEYSTONE (card a134b370-10b1-49c6-aa42-e1a05446e887): spawn the
    // account-registry publish/refresh loop alongside the IPC accept
    // loop. THIS is what makes two machines on the same gh account
    // discover and route to each other with zero human action — the
    // already-built `publish_account_registry`/`refresh_account_registry`
    // were never called on a cadence before this. The loop opens its
    // own `Airc` handle against the same machine-account home the
    // daemon owns and publishes to the gh-gist rendezvous, gated on
    // `gh auth` (optional transport — skips cleanly if unauthed).
    //
    // Shutdown shares the daemon's `Notify`: the Stop handler's
    // `notify_waiters()` wakes both the accept loop AND this loop. The
    // loop registers its waiter via the pinned `notified()` future it
    // holds internally (same lost-wakeup discipline as `server::run`).
    let registry_state = state.clone();
    let registry_home = state.home.clone();
    // Card 7e3c9a1f: the registry task binds the LAN listener on the SHARED
    // daemon handle (one adapter for accept + dial + forward) rather than
    // opening its own — that split was the reverse-broadcast bug.
    let registry_airc = daemon_airc.clone();
    // Self-healing join: the registry task fills this once the
    // rendezvous store is resolved (see `rendezvous_for_heal` above).
    let heal_slot = rendezvous_for_heal.clone();
    let registry_handle = tokio::spawn(async move {
        // HERMETIC GATE (card d793c242): test/temp daemons inherit the
        // operator's working gh auth, so without this gate they publish
        // test identities to the PRODUCTION account rendezvous (live
        // evidence: temp-scoped Windows test daemon landed in joelteply
        // gist 1214fb43d2c00d667c4712e6023b2165). Blocked scopes never
        // spawn the loop at all — ONE loud line says why. The same gate
        // is re-checked per tick and inside the gh store itself.
        if let Some(block) = airc_lib::account_registry_block(&registry_home) {
            eprintln!("airc daemon: account-registry loop DISABLED — {block}");
            return;
        }
        // Card 7e3c9a1f: the SHARED daemon handle (opened in `run_daemon`).
        // Its inbound sink (Card 4132f48c: inbound cross-machine frames
        // ingest into the owner-core router, not a private store) and its
        // forwarder link (Card 1998f6cb) are installed ONCE in `run_daemon`
        // before this task spawns; the route-refresh loop dials on this
        // same handle, so accept + dial connections share one adapter.
        let airc = registry_airc;

        // Endpoint-in-beacon (the second half of same-account
        // auto-discovery): bind a LAN listener on THIS handle so its
        // `route_endpoints()` carries a dialable address, which the
        // registry-refresh loop below then publishes in the account
        // beacon. Without this the beacon advertises `endpoints: none`
        // and a same-account peer that imports our record has nothing
        // to dial — auto-discovery enrols but never routes (validated
        // 2026-06-11: `registry sync` published with no endpoint, so the
        // Mac side could enrol 5090 but not reach it). Bind to the
        // detected LAN IP (not 0.0.0.0) so the advertised addr is the
        // one peers actually dial. Best-effort + loud: a node with no
        // routable LAN (or a bind failure) still reaches the mesh by
        // dialing OUT to listening peers / relay — it just isn't
        // dialable itself, which we say plainly rather than swallow.
        // Advertise BOTH the LAN and Tailscale addresses (the connection
        // ladder: local → LAN → Tailscale → greater-grid P2P). Tailscale
        // is unnecessary for same-subnet peers — a 192.168.x dial is
        // direct, while routing same-LAN traffic over 100.x is a wasted
        // hop. So we publish the LAN address (dialed first, no hop) AND
        // the Tailscale address (the NAT-traversing fallback). One
        // wildcard listener serves both interfaces; the endpoint sort
        // order (LanTcp before TailscaleTcp) makes the dialer try LAN
        // first and break on success.
        //
        // BIGMAMA review BLOCKING-2 on PR #1201 — honest cost note:
        // the dialer (`airc_lib::discovery::dial_stored_peer_endpoints`)
        // walks the merged endpoint list in unconditional
        // `RouteEndpointKind` order for EVERY peer — there is no
        // same-subnet/reachability gate. Off-LAN peers (i.e. on the same
        // tailnet but a different physical LAN) therefore dial the
        // publisher's unreachable 192.168.x rung FIRST and pay up to
        // `PEER_DIAL_TIMEOUT` (3s) before falling through to the live
        // Tailscale rung. Earlier this preferred Tailscale exclusively,
        // forcing every same-LAN peer through an unnecessary 100.x hop;
        // the new "LAN-first, 3s timeout, then Tailscale" cost is
        // intentional (same-LAN wins outweighs the off-LAN 3s) and
        // pinned by the dead-LAN/live-Tailscale test in
        // `airc-lib/tests/stored_endpoint_dial.rs`. A future real fix
        // (subnet/reachability gate at the dialer) will eliminate the
        // 3s for off-LAN peers; until then the truth is visible in the
        // recorded peer_dial_failures, not hidden in comments.
        // Card 7e3c9a1f: `advertise_lan_ip()` honors the `AIRC_ADVERTISE_IP`
        // handoff (host launcher / container entrypoint) before host
        // auto-detection — so a containerized node advertises a routable
        // endpoint instead of nothing (detect_lan_ip bails in a container).
        let lan_ip = crate::network_commands::advertise_lan_ip();
        let tailscale_ip = crate::network_commands::detect_tailscale_ip();
        if lan_ip.is_none() && tailscale_ip.is_none() {
            eprintln!(
                "airc daemon: no routable LAN or Tailscale IPv4 detected — account beacon \
                 carries no endpoint (outbound-dial / relay only)"
            );
        } else {
            match airc.listen_lan_advertising(lan_ip, tailscale_ip).await {
                Ok(endpoints) => {
                    let summary = endpoints
                        .iter()
                        .map(|endpoint| match endpoint {
                            airc_lib::RouteEndpoint::LanTcp { addr } => format!("LAN {addr}"),
                            airc_lib::RouteEndpoint::TailscaleTcp { addr } => {
                                format!("Tailscale {addr}")
                            }
                            other => format!("{other:?}"),
                        })
                        .collect::<Vec<_>>()
                        .join(" + ");
                    eprintln!(
                        "airc daemon: advertising {summary} in the account registry \
                         (LAN dialed first; off-LAN peers fall through to Tailscale \
                         after a ~3s LAN-rung timeout, once per session)"
                    );
                    // LAN presence beacon: same-network peers discover this
                    // node (and it discovers them) with ZERO gh requests —
                    // the gh rendezvous stays the CROSS-network path only.
                    // Non-fatal on purpose: a node that cannot join the
                    // multicast group (locked-down interface, container
                    // without multicast) still dials out and still rides the
                    // rendezvous; it says so once, loudly, instead of
                    // failing the daemon.
                    let presence_port = endpoints.iter().find_map(|endpoint| {
                        if let airc_lib::RouteEndpoint::LanTcp { addr } = endpoint {
                            Some(addr.port())
                        } else {
                            None
                        }
                    });
                    if let Some(port) = presence_port {
                        if let Err(error) = airc.start_lan_presence(port) {
                            eprintln!("airc daemon: {error}");
                        }
                    }
                    // Self-healing join — publish-on-bind: a freshly bound
                    // listener (daemon restart, possibly on a NEW port when
                    // the stable port was taken) must propagate to the
                    // rendezvous NOW, not at the refresh loop's first
                    // cadence tick. `Notify` stores the permit, so the loop
                    // below publishes immediately on start. Idempotent: an
                    // unchanged advertisement republishes the same document.
                    registry_state.endpoint_resync.notify_one();
                }
                Err(error) => {
                    eprintln!(
                        "airc daemon: LAN listener bind failed ({error}) — account beacon \
                         carries no endpoint; this node reaches the mesh by dialing out / \
                         relay but is not itself dialable"
                    );
                }
            }
        }
        // Card 4b6a0ffa (#33): record the endpoints this handle now
        // advertises into the daemon's IPC-served state, so a manual
        // `airc registry sync` can read them back over
        // `Request::RouteEndpoints` and publish a DIALABLE beacon
        // instead of an endpoint-less overwrite. Loud on failure —
        // an unreadable endpoint table is a bug, not a shrug.
        match airc.route_endpoints() {
            Ok(endpoints) => {
                *registry_state.route_endpoints.write().await = endpoints
                    .into_iter()
                    .map(crate::registry_commands::route_endpoint_to_ipc)
                    .collect();
            }
            Err(error) => {
                eprintln!(
                    "airc daemon: could not record route endpoints for IPC read-back \
                     ({error}) — `airc registry sync` will refuse endpoint-less publishes"
                );
            }
        }
        let db_path = airc_lib::machine_account_home(&registry_home).join("events.sqlite");
        let event_store = match SqliteEventStore::open_path(&db_path).await {
            Ok(store) => Arc::new(store),
            Err(error) => {
                eprintln!(
                    "airc daemon: account-registry loop disabled — could not open store: {error}"
                );
                return;
            }
        };
        // Resolve gh's full path for the daemon's own use. The gate +
        // store default to bare `gh`, but a DETACHED daemon descended
        // from a bash launcher has a PATH `Command::new("gh")` can't
        // resolve on Windows (unix-format / missing the install dir), so
        // every tick failed `gh auth status` and the rendezvous never
        // published — the real cross-machine blocker. An explicit path
        // makes gh invocable regardless of PATH shape (GH_TOKEN handles
        // the auth half via inject_gh_token).
        let gh_bin = resolve_gh_bin();
        if let Some(bin) = &gh_bin {
            eprintln!(
                "airc daemon: account-registry using gh at {}",
                bin.display()
            );
        }
        // Rendezvous SELECTION (#113): which door the mesh converges
        // through is a data choice, not a hardcode. `AIRC_RENDEZVOUS_DIR`
        // set → the no-GitHub shared-folder door (on-prem / behind
        // firewall); unset → the default gist door. The resolver pairs the
        // chosen store with the gate that matches it (gist → gh-auth,
        // folder → Always), so this loop never learns which door won.
        //
        // Stale-token recovery slot (card 1f2cbffa) rides the gist door:
        // the SAME slot goes to the gate (which re-resolves on auth
        // failure) and the store (whose gh spawns then carry the recovered
        // token), so a mid-session token rotation no longer bricks the
        // rendezvous until daemon restart. The folder door ignores it.
        let choice = match airc_lib::RendezvousChoice::from_env() {
            Ok(choice) => choice,
            Err(error) => {
                eprintln!("airc daemon: account-registry loop disabled — {error}");
                return;
            }
        };
        if let airc_lib::RendezvousChoice::Folder { dir } = &choice {
            eprintln!(
                "airc daemon: account-registry using shared-folder rendezvous at {} (no gh)",
                dir.display()
            );
        }
        let (store, gate) = airc_lib::resolve_account_registry_store(
            choice,
            airc_lib::GistRendezvous {
                event_store,
                scope_home: registry_home.clone(),
                gh_bin: gh_bin.clone(),
                token_override: airc_lib::GhTokenOverride::new(),
            },
        );
        // Self-healing join: share the resolved store + gate with the
        // route-refresh loop so a failed dial can re-read the rendezvous
        // (refresh-on-failure) instead of blindly retrying a dead
        // endpoint. `Arc<dyn>` because both loops consume the SAME
        // resolved door — resolving twice would race gh token recovery.
        let store: Arc<dyn airc_lib::AccountRegistryStore> = Arc::from(store);
        let _ = heal_slot.set((store.clone(), gate.clone()));
        airc_lib::run_registry_refresh_loop(
            airc,
            store,
            gate,
            airc_lib::RegistryRefreshConfig::default(),
            &registry_state.endpoint_resync,
            // #240 event-driven heal: nudge the route-refresh loop the
            // instant an import lands fresh endpoints so they are dialed
            // NOW, not up to a full route-refresh interval later.
            &registry_state.route_wake,
            registry_state.shutdown.notified(),
        )
        .await;
    });

    run_daemon_server(state, socket).await?;
    // The route-refresh loop exits on the shutdown `Notify` that ended
    // the accept loop; abort is the backstop for the listener-error
    // path, where Stop never fired (same abort discipline as
    // `HeartbeatTask::stop`).
    route_refresh_task.abort();
    // #1268: the auto-update loop also exits on the shared shutdown notifier;
    // abort is the same listener-error backstop. (The detached updater it may
    // have spawned is independent and intentionally outlives this process.)
    auto_update_task.abort();
    // Server returned ⇒ shutdown fired ⇒ the registry loop's shutdown
    // waiter was woken by the same `notify_waiters()`. Await its clean
    // exit so the process doesn't drop an in-flight gist write
    // mid-flight.
    let _ = registry_handle.await;
    println!("airc daemon: stopped.");
    Ok(())
}

/// Card 625abe6d slice 2 — daemon-resident continuous route
/// discovery. `refresh_route_discovery` (slice 1) dials every
/// enrolled peer's stored endpoints outbound; this task calls it on
/// the daemon clock (`route_refresh::FIRST_REFRESH_DELAY` after
/// start, then every `route_refresh::REFRESH_INTERVAL`) so stored-
/// endpoint dials and route health are continuous — sleep/wake and
/// daemon restarts re-establish routes with zero operator action,
/// instead of waiting for someone to run `airc transport health`.
///
/// Card 7e3c9a1f: the route-refresh loop dials on the SHARED daemon
/// handle (the same one that binds the listener and is registered with
/// the forwarder). Accepted (inbound) and dialed (outbound) connections
/// therefore live on ONE `LanTcpAdapter` connection map, so the
/// forwarder and the delivery-ack path can reach a peer regardless of
/// which side opened the link — the fix for reverse-direction room
/// broadcast. The handle is kept for the daemon's lifetime: LAN
/// connections live on its adapter, so re-opening per tick would sever
/// them. Inbound sink + forwarder link are installed once in
/// `run_daemon` before this spawns.
/// Self-healing join: the daemon's ONE resolved rendezvous (store +
/// gate), filled by the registry task, read by the route-refresh loop's
/// refresh-on-failure heal. `OnceLock` because resolution happens
/// exactly once and the readers only ever need "the resolved door or
/// nothing yet".
type SharedRendezvousSlot = std::sync::OnceLock<(
    Arc<dyn airc_lib::AccountRegistryStore>,
    airc_lib::RegistryRefreshGate,
)>;

fn spawn_route_refresh(
    state: Arc<DaemonState>,
    airc: Airc,
    rendezvous: Arc<SharedRendezvousSlot>,
) -> tokio::task::JoinHandle<()> {
    let connected = state.connected_lan_peers.clone();
    let delivery_stats = state.delivery_stats.clone();
    let endpoint_resync = state.endpoint_resync.clone();
    tokio::spawn(async move {
        airc_daemon::route_refresh::run_periodic_refresh(
            &state.shutdown,
            &state.route_wake,
            || {
                refresh_routes_once(
                    &airc,
                    &connected,
                    &delivery_stats,
                    &endpoint_resync,
                    &rendezvous,
                )
            },
        )
        .await;
    })
}

/// One periodic route refresh: run discovery on the shared daemon handle
/// (which dials stored peer endpoints outbound, 3s-bounded each), and
/// surface every failure through the diagnostic sink — loud, never
/// silent. Failures never propagate: the loop's next tick is the retry
/// path (self-heal doctrine, card 625abe6d).
async fn refresh_routes_once(
    airc: &Airc,
    connected_lan_peers: &std::sync::atomic::AtomicUsize,
    delivery_stats: &tokio::sync::RwLock<Vec<airc_ipc::IpcPeerDeliveryStats>>,
    endpoint_resync: &tokio::sync::Notify,
    rendezvous: &SharedRendezvousSlot,
) {
    // Adaptable-router reflex: re-detect this node's own routable LAN +
    // Tailscale IPv4 every tick (cheap, local — no network) and re-advertise
    // if it moved (router swap, DHCP renew, Tailscale toggle). Without this
    // the endpoint computed once at daemon start is frozen, so a node that
    // changes IP keeps advertising a stale, undialable address until it is
    // manually restarted. Reused below for relay self-election so detection
    // happens exactly once per tick.
    let lan_ip = crate::network_commands::advertise_lan_ip();
    let tailscale_ip = crate::network_commands::detect_tailscale_ip();
    match airc
        .refresh_advertised_endpoints(lan_ip, tailscale_ip)
        .await
    {
        Ok(true) => {
            // Changed → nudge the registry loop to republish the corrected
            // card NOW (edge-triggered; steady-state stays on cadence, so
            // no spam). Loud: a reachability-affecting change is not silent.
            let summary = airc
                .route_endpoints()
                .map(|endpoints| {
                    endpoints
                        .iter()
                        .map(|endpoint| format!("{endpoint:?}"))
                        .collect::<Vec<_>>()
                        .join(" + ")
                })
                .unwrap_or_else(|_| "<unreadable>".to_string());
            eprintln!(
                "airc daemon: advertised endpoint changed (LAN/Tailscale IP moved) — \
                 now advertising [{summary}]; resyncing the account-registry card"
            );
            endpoint_resync.notify_one();
        }
        Ok(false) => {}
        Err(error) => {
            eprintln!(
                "airc daemon: advertised-endpoint refresh failed ({error}); retrying next tick"
            );
        }
    }
    match airc.refresh_route_discovery().await {
        Ok(mut snapshot) => {
            for failure in &snapshot.peer_dial_failures {
                StderrJsonDiagnosticSink.emit(
                    DiagnosticEvent::warn(
                        DiagnosticComponent::Daemon,
                        DiagnosticCode::PeerDialFailed,
                        "stored peer endpoint did not answer a route-discovery dial",
                    )
                    .with_field("peer_id", failure.peer_id)
                    .with_field("endpoint", format!("{:?}", failure.endpoint))
                    .with_field("error", &failure.error),
                );
            }

            // Self-healing join — refresh-on-failure: dials failed, so
            // re-read the rendezvous for FRESHER endpoints for those
            // peers before their next blind retry (the M5↔bigmama
            // stale-port decay: the old port stays dead forever unless
            // someone re-reads the advertisement). Gated exactly like
            // the registry loop's own ticks; when a fresher endpoint
            // arrived, the heal already re-dialed it and the healed
            // snapshot replaces this tick's (so the connected count and
            // relay self-election below see the post-heal truth).
            if !snapshot.peer_dial_failures.is_empty() {
                if let Some((store, gate)) = rendezvous.get() {
                    if gate.block().await.is_none() {
                        match airc
                            .heal_failed_dials(store.as_ref(), &snapshot.peer_dial_failures)
                            .await
                        {
                            Ok(Some(healed)) => {
                                eprintln!(
                                    "airc daemon: dial failure(s) triggered a rendezvous re-read; \
                                     fresher endpoint(s) found and re-dialed — connected LAN \
                                     peers now: {}",
                                    healed.connected_lan_peers.len()
                                );
                                snapshot = healed;
                            }
                            Ok(None) => {}
                            Err(error) => {
                                StderrJsonDiagnosticSink.emit(
                                    DiagnosticEvent::warn(
                                        DiagnosticComponent::Daemon,
                                        DiagnosticCode::RouteRefreshFailed,
                                        "refresh-on-failure rendezvous re-read failed; dead \
                                         endpoints stay in backoff until the next cycle",
                                    )
                                    .with_field("error", error),
                                );
                            }
                        }
                    }
                }
            }

            // Publish the live LAN-peer count for `Status` to report —
            // the set room broadcast actually fans out to. This is the
            // ONLY writer (the daemon crate can't reach the airc-lib
            // handle that owns the connections), refreshed every tick so
            // `airc send` can warn loudly when a broadcast reaches no one.
            connected_lan_peers.store(
                snapshot.connected_lan_peers.len(),
                std::sync::atomic::Ordering::Relaxed,
            );

            // #1306 slice 2: publish the delivery-ledger snapshot for
            // `Request::DeliveryStats` — the delivery-truth read behind
            // doctor's "last confirmed delivery to X: N ago". Same
            // single-writer wiring split as the counter above (the
            // daemon crate can't reach the airc-lib ledger).
            if let Some(ledger) = airc.delivery_ledger() {
                let rows = ledger
                    .snapshot()
                    .into_iter()
                    .map(|(peer_id, stats)| airc_ipc::IpcPeerDeliveryStats {
                        peer_id,
                        attempts: stats.attempts,
                        acked: stats.acked,
                        attempts_since_ack: stats.attempts_since_ack,
                        last_attempt_ms: stats.last_attempt_ms,
                        last_ack_ms: stats.last_ack_ms,
                        rtt_ema_ms: stats.rtt_ema_ms,
                        suspect: stats.suspect(),
                    })
                    .collect::<Vec<_>>();
                *delivery_stats.write().await = rows;
            }

            // #1247 slice 4b — relay self-election. When this node can
            // reach no peer directly AND has no live relay yet, but knows
            // enrolled peers exist, it promotes itself to a relay (the
            // "be a relay" mechanism is `Airc::become_relay`, slice 4a).
            // The next account-registry publish carries the advertised
            // relay endpoint into the gist, so reachable peers discover +
            // connect to it (slices 2-3). Idempotent + empirical: a node
            // already relaying just re-advertises, and an unreachable
            // self-elected relay is harmlessly ignored (no connections →
            // stale gist entry), so no need to predict our own
            // reachability. Binds all interfaces on an OS-assigned port —
            // the gist advertises whatever port the OS gave, so the
            // durable pointer stays valid across restarts.
            let enrolled = airc.peers().await.map(|peers| peers.len()).unwrap_or(0);
            // #267 follow-up (2026-07-31 regression): relay-hood is a
            // DURABLE ROLE, not an emergency fallback. Once this node has
            // ever been a relay, peers hold `airc-relay://me@ip:port`
            // cards — if a daemon restart only re-elects when NO peer is
            // reachable, every card-holder gets Connection refused while
            // this node chats happily over its one direct LAN link
            // (glass-boxed live: overnight restart dropped :65280 while
            // .232 stayed connected). A persisted relay-port file IS the
            // role record: re-assume it on every tick unconditionally
            // (become_relay is idempotent — already-relaying is a cheap
            // re-advertise).
            let has_relay_role = read_persisted_relay_port().is_some();
            if snapshot.should_self_elect_as_relay(enrolled) || has_relay_role {
                // Slice 4c: advertise the relay under our ROUTABLE IP(s)
                // (LAN + Tailscale), never the 0.0.0.0 bind — peers can't
                // dial a wildcard. Reuses the IPs detected once at the top
                // of this tick (same source the endpoint self-heal uses).
                // With neither IP, we'd be an un-dialable relay, so stay a
                // client + say why (loud).
                if lan_ip.is_none() && tailscale_ip.is_none() {
                    eprintln!(
                        "airc daemon: would self-elect as a relay (no reachable peer/relay, \
                         enrolled={enrolled}) but no routable LAN/Tailscale IPv4 to advertise \
                         — staying a client (open a routable interface to host a relay)"
                    );
                } else {
                    // #267: prefer the port the LAST relay incarnation bound
                    // (persisted in the runtime dir). An OS-assigned port
                    // drifts on every daemon restart, and remote peers'
                    // imported `airc-relay://me@ip:port` cards republish on
                    // THEIR cadence — so every restart stranded the mesh on
                    // a dead port (glass-boxed live: peers dialing :65280
                    // while the new relay sat on :57958, 0 healthy routes).
                    // Re-binding the persisted port keeps every stale card
                    // valid; OS-assigned is only the first-election / port-
                    // stolen fallback, and whatever binds gets persisted.
                    match become_relay_with_stable_port(airc, lan_ip, tailscale_ip).await {
                        Ok(addr) => {
                            eprintln!(
                                "airc daemon: no reachable peer or relay (enrolled={enrolled}) — \
                                 self-elected as a relay (listening {addr}) and advertised it on \
                                 this node's routable IP(s) (#1247)"
                            );
                            // Self-healing join — publish-on-bind: the relay
                            // listener just bound; propagate the new relay
                            // endpoint to the rendezvous now instead of
                            // waiting up to a full registry cadence.
                            endpoint_resync.notify_one();
                        }
                        Err(error) => eprintln!(
                            "airc daemon: relay self-election failed ({error}); \
                             retrying next interval"
                        ),
                    }
                }
            }
        }
        Err(error) => {
            StderrJsonDiagnosticSink.emit(
                DiagnosticEvent::error(
                    DiagnosticComponent::Daemon,
                    DiagnosticCode::RouteRefreshFailed,
                    "periodic route-discovery refresh failed; retrying next interval",
                )
                .with_field("error", error),
            );
        }
    }
}

/// #267: where this machine's relay listener port persists across daemon
/// restarts — machine-scope (one relay per node), beside the daemon socket.
fn relay_port_file() -> Option<std::path::PathBuf> {
    crate::runtime_dir::runtime_dir()
        .ok()
        .map(|dir| dir.join("relay-port"))
}

fn read_persisted_relay_port() -> Option<u16> {
    std::fs::read_to_string(relay_port_file()?)
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn persist_relay_port(port: u16) {
    if let Some(path) = relay_port_file() {
        // Best-effort: a failed persist only costs port stability on the
        // NEXT restart, never this election.
        let _ = std::fs::write(path, port.to_string());
    }
}

/// Bind order for relay self-election: the persisted previous port first
/// (keeps every peer's imported `airc-relay://me@ip:port` card valid across
/// restarts), OS-assigned as the first-election / port-stolen fallback.
fn relay_bind_candidates(persisted: Option<u16>) -> Vec<u16> {
    match persisted {
        Some(port) if port != 0 => vec![port, 0],
        _ => vec![0],
    }
}

/// Self-elect as a relay on a RESTART-STABLE port (#267). Tries the
/// persisted previous port, falls back to OS-assigned, and persists
/// whatever actually bound so the next incarnation lands on it again.
// AircError is >=128B (clippy 1.98 result_large_err); this is a cold
// startup path, not a hot loop — scoped allow until AircError's large
// variants are boxed library-wide.
#[allow(clippy::result_large_err)]
async fn become_relay_with_stable_port(
    airc: &Airc,
    lan_ip: Option<std::net::Ipv4Addr>,
    tailscale_ip: Option<std::net::Ipv4Addr>,
) -> Result<std::net::SocketAddr, airc_lib::AircError> {
    // Sticky candidates first; the OS-assigned bind (port 0, always the
    // final candidate — pinned by the relay_bind_candidates tests) is the
    // TERMINAL attempt whose error propagates directly, so no empty-list
    // panic path exists (CI denies clippy::expect_used).
    for port in relay_bind_candidates(read_persisted_relay_port()) {
        if port == 0 {
            continue;
        }
        if let Ok(addr) = airc
            .become_relay(
                std::net::SocketAddr::from(([0, 0, 0, 0], port)),
                lan_ip,
                tailscale_ip,
            )
            .await
        {
            persist_relay_port(addr.port());
            return Ok(addr);
        }
    }
    let addr = airc
        .become_relay(
            std::net::SocketAddr::from(([0, 0, 0, 0], 0)),
            lan_ip,
            tailscale_ip,
        )
        .await?;
    persist_relay_port(addr.port());
    Ok(addr)
}

fn current_daemon_runtime_info() -> DaemonRuntimeInfo {
    DaemonRuntimeInfo {
        ipc_protocol_version: Some(u32::from(airc_ipc::IPC_PROTOCOL_VERSION)),
        build_commit: (!crate::build_info::is_unknown()).then(|| crate::build_info::COMMIT.into()),
        build_branch: (!crate::build_info::is_unknown()).then(|| crate::build_info::BRANCH.into()),
        executable: std::env::current_exe()
            .ok()
            .map(|path| path.display().to_string()),
    }
}

// ---- Daemon-client commands (no identity load needed) ---------------

pub async fn run_ping(socket: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let client = DaemonClient::new(socket);
    client.ping().await?;
    println!("pong");
    Ok(())
}

/// `status` — daemon health snapshot.
///
/// Card 2bdae532: regression-fix. Earlier builds auto-spawned the
/// daemon if the socket wasn't reachable, so `airc status` doubled as
/// a "make the daemon ready" command. The current binary had lost
/// that, so a fresh attach (cargo install then airc status) failed
/// with "daemon not reachable: No such file or directory" with no
/// next step — Codex hit this on first onboard 2026-05-28. Restoring
/// `ensure_daemon_running` before the probe gives every recipe that
/// says "run `airc status` first" a working contract again.
pub async fn run_status(home: &Path, socket: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    // `ensure_daemon_running` may SPAWN a daemon (see card 2bdae532 above), which
    // makes this command unable to report "down" — asking creates the answer.
    // That is fine as convenience and fatal as a measurement, so establish
    // whether one was already answering BEFORE we ensure, and say so.
    //
    // Incident 2026-08-08: a grid blackout drill measured a 0.0s outage across
    // an `airc update` because its probe was `airc status`. The positive control
    // failed — status reported UP one second after `airc stop` — and the number
    // was retracted. An instrument that changes what it measures reads as
    // healthy precisely when the thing it watches is broken.
    //
    // Worse than a bad number: anything polling `status` DURING an update
    // respawns a daemon from the OLD binary mid-swap, which is a candidate cause
    // of the stale-process class `update_commands.rs` already guards against
    // ("a stale process that survived `stop_daemon` answers IPC perfectly").
    //
    // The spawn is NOT removed — 2bdae532 added it so a fresh onboard has a
    // working contract, and breaking onboarding to fix an honesty bug trades one
    // defect for another. It is now DISCLOSED instead, and `airc ping` remains
    // the non-spawning probe for anyone who needs to observe rather than ensure.
    let was_already_up = DaemonClient::new(socket.clone())
        .status_with_timeout(Duration::from_millis(250))
        .await
        .is_ok();

    ensure_daemon_running(home, socket.clone(), Vec::new()).await?;
    let client = DaemonClient::new(socket);
    let status = client.status().await?;
    if !was_already_up {
        println!(
            "note: no daemon was answering — this command STARTED one. \
             It was not running until you asked. Use `airc ping` to observe \
             liveness without starting anything."
        );
    }
    println!("peer_id:        {}", status.peer_id);
    println!("uptime_seconds: {}", status.uptime_seconds);
    if let Some(version) = status.ipc_protocol_version {
        println!("ipc_protocol:   {version}");
    }
    if let Some(commit) = status.build_commit.as_deref() {
        let short = &commit[..commit.len().min(12)];
        println!("build:          {short}");
    }
    if let Some(branch) = status.build_branch.as_deref() {
        println!("branch:         {branch}");
    }
    if let Some(executable) = status.executable.as_deref() {
        println!("executable:     {executable}");
    }
    // Joel's ruling 2026-08-08: the version must be visible on every
    // health/query surface, in every repo, because stale binaries have
    // repeatedly poisoned testing. `build:` above reports what the DAEMON says
    // it is; this reports what the CLI asking the question is. Printing both is
    // the point — when they disagree, the operator is talking to a daemon built
    // from different source than the tool they are holding, which is precisely
    // the confusion the #354 incident produced ("Already at 1e2f424" one line
    // above `airc --version` saying three commits behind, both true, describing
    // different objects).
    println!("cli_version:    {}", crate::build_info::version_line());
    Ok(())
}

pub async fn run_stop(socket: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let client = DaemonClient::new(socket);
    client.stop().await?;
    println!("daemon: stop requested.");
    Ok(())
}

pub async fn run_msg(
    home: &Path,
    socket: PathBuf,
    room: Option<&str>,
    text: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let socket = ensure_daemon_running(home, socket, Vec::new()).await?;
    sync_daemon_peers_for_current_rooms(home, socket.clone()).await?;
    let airc = Airc::attach(home, socket).await?;
    // Card a979e5c2 (seam #5): `--room <name>` routes ONE message
    // to a subscribed-but-not-current room without mutating this
    // scope's default-room pointer. Same shape as `airc publish`.
    // Without `--room`, the historical "current room" path runs
    // unchanged.
    let (channel_name, channel) = match room {
        Some(name) => {
            let receipt = airc
                .publish(
                    airc_lib::PublishTarget::RoomByName(name.to_string()),
                    airc_protocol::FrameKind::Message,
                    airc_core::Body::text(text),
                    runtime_headers()?,
                )
                .await?;
            (receipt.channel_name, receipt.channel_id)
        }
        None => {
            let current = airc.current_room().await?;
            airc.say_with_headers(text, runtime_headers()?).await?;
            (current.name, current.channel)
        }
    };
    let channel_id = channel.to_string();
    // Same enrolled-vs-delivered honesty fix as run_send, for the
    // daemon-attached send path. `peers()` is the address book, not a
    // delivery receipt; the daemon's live-connection count (Status) is
    // the set room broadcast can actually reach.
    let peer_count = airc.peers().await?.len();
    let connected_lan_peers = DaemonClient::new(crate::cli::default_socket_path_in(home))
        .status()
        .await
        .map(|status| status.connected_lan_peers)
        .unwrap_or(0);
    println!(
        "{}",
        format_send_receipt(&channel_name, &channel_id, peer_count, connected_lan_peers)
    );
    if let Some(warning) = mention_audience_warning(&airc, text, channel, &channel_name).await {
        println!("{warning}");
    }
    Ok(())
}

pub async fn run_inbox(
    home: &Path,
    socket: Option<PathBuf>,
    room: Option<&str>,
    since_lamport: Option<u64>,
    since_event_id: Option<String>,
    limit: Option<usize>,
    as_json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = match socket {
        Some(socket) => {
            let socket = ensure_daemon_running(home, socket, Vec::new()).await?;
            Airc::attach(home, socket).await?
        }
        None => attached_airc(home).await?,
    };
    // `--room` reads a room this scope is subscribed to WITHOUT moving
    // the default-room pointer — the read sibling of `airc msg --room`.
    // Same resolver the writes use (name or channel id, loud refusal for
    // an unsubscribed room, never auto-joins), so a room `msg --room` can
    // reach is exactly a room `inbox --room` can read.
    let room = match room {
        Some(name) => airc.room_by_name_or_channel(name, "read").await?,
        None => airc.current_room().await?,
    };
    // Both --since-lamport and --since-event-id must be supplied
    // together; the cursor is a tuple per grievance §7.
    let since = match (since_lamport, since_event_id) {
        (Some(lamport), Some(ref ev)) => Some(TranscriptCursor {
            lamport,
            event_id: EventId::from_uuid(uuid::Uuid::parse_str(ev)?),
        }),
        (None, None) => None,
        _ => {
            return Err(
                "--since-lamport and --since-event-id must be passed together (cursor is a tuple)"
                    .into(),
            );
        }
    };
    let effective_limit = limit.unwrap_or(32);
    let events = match since {
        Some(cursor) => airc.resume_from_in(&room, &cursor, effective_limit).await?,
        None => airc.page_recent_in(&room, effective_limit).await?,
    };
    // #270: `inbox` reads ONE room — say so, loudly, and name what it is
    // NOT showing. The unlabeled view is how "your message isn't in my
    // inbox" became a false transport diagnosis twice in one day: the
    // message was in the store the whole time, in a subscribed room that
    // wasn't current.
    //
    // The remedy this prints is now `--room <name>`, not `airc room
    // <name>`: reading another room must not require MOVING this scope's
    // default-room pointer. Telling the operator to switch rooms in order
    // to read one was the bug wearing a label.
    if !as_json {
        if let Ok(set) = airc.subscription_set().await {
            let others: Vec<String> = set
                .all()
                .filter(|s| s.name.as_str() != room.name)
                .map(|s| s.name.as_str().to_string())
                .collect();
            if others.is_empty() {
                println!("inbox: room '{}' (your only subscribed room)", room.name);
            } else {
                println!(
                    "inbox: room '{}' ONLY — {} other subscribed room(s) NOT shown: {} \
                     (read one with `airc inbox --room <name>`)",
                    room.name,
                    others.len(),
                    others.join(", ")
                );
            }
            println!();
        }
    }
    if as_json {
        print_inbox_json(&events)?;
        return Ok(());
    }
    if events.is_empty() {
        println!("(no events)");
        return Ok(());
    }
    for event in &events {
        print_event(event);
    }
    if let Some(cursor) = events.last().map(airc_core::TranscriptEvent::cursor) {
        println!();
        println!(
            "cursor: lamport={} event_id={} — pass both as --since-lamport / --since-event-id",
            cursor.lamport, cursor.event_id
        );
    }
    Ok(())
}

/// Emit a single JSON document for `airc inbox --json`.
///
/// Shape: `{ count, events, cursor: {lamport, event_id} | null }`.
/// The cursor is the paging hint pointing at the newest event in
/// this page; pass both halves back as `--since-lamport` /
/// `--since-event-id` for the next call. Mirrors `airc events
/// list --json` for the `{count, events}` shape and extends it
/// with the paging tuple inbox callers need.
fn print_inbox_json(events: &[airc_core::TranscriptEvent]) -> Result<(), serde_json::Error> {
    #[derive(serde::Serialize)]
    struct InboxJson<'a> {
        count: usize,
        events: &'a [airc_core::TranscriptEvent],
        cursor: Option<InboxCursorJson>,
    }
    #[derive(serde::Serialize)]
    struct InboxCursorJson {
        lamport: u64,
        event_id: String,
    }

    let cursor = events
        .last()
        .map(airc_core::TranscriptEvent::cursor)
        .map(|cursor| InboxCursorJson {
            lamport: cursor.lamport,
            event_id: cursor.event_id.to_string(),
        });
    println!(
        "{}",
        serde_json::to_string_pretty(&InboxJson {
            count: events.len(),
            events,
            cursor,
        })?
    );
    Ok(())
}

async fn print_event_stream_until_signal<S>(
    stream: &mut S,
) -> Result<(), Box<dyn std::error::Error>>
where
    S: futures::stream::Stream<
            Item = Result<std::sync::Arc<airc_core::TranscriptEvent>, airc_lib::LiveLag>,
        > + Unpin,
{
    let sigint = tokio::signal::ctrl_c();
    let mut sigint = Box::pin(sigint);
    loop {
        tokio::select! {
            biased;
            _ = &mut sigint => {
                println!();
                println!("interrupted; exiting.");
                return Ok(());
            }
            next = stream.next() => {
                match next {
                    Some(Ok(event)) => print_event(&event),
                    Some(Err(lag)) => {
                        // LiveLag is the explicit signal that the
                        // consumer fell behind broadcast capacity.
                        // Print and continue — the operating doc
                        // says lag must surface, not silently drop.
                        eprintln!("{lag}");
                    }
                    None => {
                        println!("stream closed; exiting.");
                        return Ok(());
                    }
                }
            }
        }
    }
}

fn print_event(event: &airc_core::TranscriptEvent) {
    // Structured events render by kind; `alive` heartbeats are suppressed
    // (None) so they don't drown the feed. See `event_render`.
    if let Some(line) = crate::event_render::render_feed_line(event) {
        println!("{line}");
    }
}

/// Build the runtime `PeerKeyRegistry` from persistent peers
/// (store-backed peer trust) + ad-hoc `--peer` flags. Self is always
/// enroled. Ad-hoc unions on top of persistent — if the same peer_id
/// appears in both, the ad-hoc pubkey wins (matches "this invocation
/// is authoritative" intuition).
async fn build_combined_registry(
    home: &Path,
    identity: &LocalIdentity,
    adhoc: &[PeerSpec],
) -> Result<Arc<PeerKeyRegistry>, Box<dyn std::error::Error>> {
    let registry = PeerKeyRegistry::new();
    registry.enrol(identity.peer_id, 0, identity.keypair.public_bytes())?;
    for stored in peers_store::load(home).await? {
        registry.enrol(stored.peer_id, 0, stored.pubkey_bytes()?)?;
    }
    for spec in adhoc {
        registry.enrol(spec.peer_id, 0, spec.pubkey)?;
    }
    Ok(Arc::new(registry))
}

/// `peer add <spec>` — persist a peer to the trust store via
/// `Airc::add_peer`. If a daemon is running on the given socket,
/// also tells it via the AddPeer RPC so the in-memory registry
/// stays in sync.
pub async fn run_peer_add(
    home: &Path,
    spec: PeerSpec,
    socket: PathBuf,
    tier: Option<airc_store::TrustTier>,
    endpoints: Vec<airc_lib::RouteEndpoint>,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = Airc::open(home).await?;
    let pubkey_b64 = base64::Engine::encode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        spec.pubkey,
    );
    let peer_id = spec.peer_id;
    airc.add_peer(spec).await?;
    // Card 34942ec1 Sub-C: --tier override. If unset, the substrate
    // default (Untrusted) from Sub-A applies — no surface change for
    // existing callers. If set, promote the freshly-added row to the
    // requested tier in a separate set_peer_trust_tier call. The
    // two-step write isn't atomic at the SQL level but the
    // substrate's invariant is "tier is orthogonal to key material"
    // — a peer briefly visible at Untrusted before the promotion
    // commits is the same state any honest peer starts at.
    if let Some(tier) = tier {
        airc_trust::set_tier(home, peer_id, tier)
            .await?
            .ok_or_else(|| {
                format!(
                    "internal: just-added peer {peer_id} missing during tier-set — \
                     report this as a substrate bug"
                )
            })?;
        println!("enroled peer_id={peer_id} (pubkey 32 bytes) tier={tier}");
    } else {
        println!("enroled peer_id={peer_id} (pubkey 32 bytes) tier=untrusted (default)");
    }

    // Card 625abe6d slice 1: persist advertised endpoints alongside
    // the trust anchor. Same two-step shape (and same justification)
    // as the tier write above. Dial happens at route discovery time
    // (`airc transport health`, daemon refresh), not here — `peer add`
    // stays a pure enrolment verb.
    if !endpoints.is_empty() {
        let endpoints_json = airc_lib::endpoints_to_json(&endpoints)
            .map_err(|error| format!("encoding --endpoint values: {error}"))?;
        // Self-healing join: an operator-supplied endpoint is fresh AS
        // OF NOW — stamp it so it outranks any stale stored set, and so
        // a later fresher advertisement can in turn replace it.
        let advertised_at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .map_err(|error| format!("system clock before epoch: {error}"))?;
        // Operator `peer add --endpoint` names the peer it dials — the
        // endpoints answer as that peer itself (no host mapping).
        airc_trust::set_endpoints_json(home, peer_id, Some(endpoints_json), advertised_at_ms, None)
            .await?
            .ok_or_else(|| {
                format!(
                    "internal: just-added peer {peer_id} missing during endpoint-set — \
                     report this as a substrate bug"
                )
            })?;
        println!(
            "stored {} endpoint(s) for {peer_id}; route discovery will dial outbound.",
            endpoints.len()
        );
    }

    // Best-effort daemon sync. If the daemon isn't running, that's
    // fine — it'll pick up the trust store on next start.
    let client = DaemonClient::new(socket);
    match client
        .call_with_timeout(
            Request::AddPeer(AddPeerRequest {
                peer_id,
                pubkey_b64,
            }),
            Duration::from_millis(250),
        )
        .await
    {
        Ok(Response::Ok) => println!("daemon: in-memory registry updated."),
        Ok(other) => println!("daemon: skipped in-memory registry sync ({other:?})."),
        Err(_) => {
            println!("daemon: not running (trust store updated; daemon will load on next start).")
        }
    }
    Ok(())
}

/// `peer remove <peer-id>` — remove a peer from durable trust and
/// update the running daemon's verifier when present.
pub async fn run_peer_remove(
    home: &Path,
    peer_id: airc_core::PeerId,
    socket: PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = Airc::open(home).await?;
    let removed = airc.remove_peer(peer_id, "manual").await?;
    if removed {
        println!("removed peer_id={peer_id}");
    } else {
        println!("peer_id={peer_id} was not enroled");
    }

    let client = DaemonClient::new(socket);
    match client
        .call_with_timeout(
            Request::RemovePeer(RemovePeerRequest { peer_id }),
            Duration::from_millis(250),
        )
        .await
    {
        Ok(Response::Ok) => println!("daemon: in-memory registry updated."),
        Ok(other) => println!("daemon: skipped in-memory registry sync ({other:?})."),
        Err(_) => {
            println!("daemon: not running (trust store updated; daemon will load on next start).")
        }
    }
    Ok(())
}

/// `peer prune` — evict DEAD trust-store enrolments (the peer-store
/// analog of `registry gc`). Removes peers that are `Untrusted` AND
/// absent from the current fresh account registry; trusted peers (incl.
/// cross-grid Friends) and live peers are never touched. Dry-run by
/// default; `--apply` performs the eviction with a forensic reason on
/// the `PeerDeparted` event.
///
/// SAFETY: if a trustworthy fresh live-peer set cannot be established (gh
/// unauthenticated, hermetic scope, unreachable rendezvous, or an empty
/// registry), this prunes NOTHING — pruning against an unknown/empty live
/// set would wrongly evict live peers.
/// Seam #3.2 operator override for the peer-prune staleness grace
/// window. Omit (`None`) → the 1h substrate default; an explicit value
/// (incl. `0` = no grace, evict every absent untrusted peer) tunes how
/// long an absent untrusted peer is kept before it ages out. A CLI flag,
/// not an env var — substrate thresholds are explicit, not ambiently
/// tuned. `saturating_mul` so an absurd hour count can't overflow.
fn resolve_stale_after_ms(stale_after_hours: Option<u64>) -> u64 {
    match stale_after_hours {
        Some(hours) => hours.saturating_mul(3_600_000),
        None => airc_lib::DEFAULT_PEER_STALE_AFTER_MS,
    }
}

pub async fn run_peer_prune(
    home: &Path,
    apply: bool,
    stale_after_hours: Option<u64>,
) -> Result<(), Box<dyn std::error::Error>> {
    let stale_after_ms = resolve_stale_after_ms(stale_after_hours);
    let enrolled = airc_trust::load(home).await?;
    if enrolled.is_empty() {
        println!("(no enroled peers — nothing to prune)");
        return Ok(());
    }
    // Seam #3.2: carry each peer's last_seen so the classifier can apply
    // the staleness grace window (a recently-contacted peer absent from
    // one registry snapshot is kept, not evicted as a ghost).
    let enrolled_triples: Vec<(airc_core::PeerId, airc_lib::TrustTier, u64)> = enrolled
        .iter()
        .map(|p| (p.peer_id, p.tier, p.last_seen_ms))
        .collect();

    // Authoritative live set: the fresh, stale-pruned account-registry
    // document (READ-ONLY — `live_registry_peer_ids` does not enrol).
    let db_path = airc_lib::machine_account_home(home).join("events.sqlite");
    let event_store = Arc::new(SqliteEventStore::open_path(&db_path).await?);
    let store = match resolve_gh_bin() {
        Some(bin) => airc_lib::GhAccountRegistryStore::new(event_store, home).with_bin(bin),
        None => airc_lib::GhAccountRegistryStore::new(event_store, home),
    };
    let airc = Airc::open(home).await?;
    let live_ids = match airc.live_registry_peer_ids(&store).await {
        Ok(Some(ids)) if !ids.is_empty() => ids,
        _ => {
            println!(
                "peer prune: could not establish a fresh live-peer set (gh not authenticated, \
                 hermetic scope, unreachable rendezvous, or empty registry). Pruning NOTHING — \
                 prune needs a trustworthy live set so it never evicts a live peer. Run `gh auth \
                 login`, ensure the account rendezvous is reachable, then retry."
            );
            return Ok(());
        }
    };

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    println!(
        "peer prune: staleness grace = {} hour(s){}",
        stale_after_ms / 3_600_000,
        if stale_after_hours.is_some() {
            " (operator override)"
        } else {
            " (default)"
        }
    );
    let verdicts =
        airc_lib::classify_peer_prune(&enrolled_triples, &live_ids, now_ms, stale_after_ms);
    let mut to_evict = Vec::new();
    for verdict in &verdicts {
        match verdict.action {
            airc_lib::PeerPruneAction::Evict => {
                println!(
                    "  EVICT  {}  tier={}  — {}",
                    verdict.peer_id,
                    verdict.tier.as_wire_str(),
                    verdict.reason
                );
                to_evict.push(verdict.peer_id);
            }
            airc_lib::PeerPruneAction::Keep => {
                println!(
                    "  keep   {}  tier={}  — {}",
                    verdict.peer_id,
                    verdict.tier.as_wire_str(),
                    verdict.reason
                );
            }
        }
    }
    let kept = verdicts.len() - to_evict.len();

    if !apply {
        if to_evict.is_empty() {
            println!("peer prune: clean — {kept} live/trusted peer(s), no dead enrolments.");
        } else {
            println!(
                "peer prune (dry run): would evict {} dead enrolment(s), keep {kept}. \
                 Re-run with --apply to evict.",
                to_evict.len()
            );
        }
        return Ok(());
    }

    let mut evicted = 0usize;
    for peer_id in &to_evict {
        if airc
            .remove_peer(
                *peer_id,
                "peer-prune: untrusted + absent from fresh registry + stale past last_seen TTL",
            )
            .await?
        {
            evicted += 1;
        }
    }
    println!("peer prune: evicted {evicted} dead enrolment(s), kept {kept}.");
    Ok(())
}

/// Card 34942ec1 Sub-C: update an enrolled peer's tier without
/// touching key material. Refuses for unknown peers (no implicit
/// add — the operator should `peer add <spec> --tier=…` instead).
/// Idempotent for no-op transitions.
pub async fn run_peer_set_tier(
    home: &Path,
    peer_id: airc_core::PeerId,
    tier: airc_store::TrustTier,
    socket: PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    // Look up the prior tier so the output can name both old + new
    // (operator audit trail) AND so the idempotent no-op path can
    // honestly say "no change."
    let prior = airc_trust::load(home)
        .await?
        .into_iter()
        .find(|p| p.peer_id == peer_id);
    let Some(prior) = prior else {
        return Err(format!(
            "peer {peer_id} is not enrolled in this scope's trust store. \
             Use `airc peer add <spec> --tier={tier}` to enrol fresh, \
             or check `airc peer list` for the right peer_id."
        )
        .into());
    };
    if prior.tier == tier {
        println!("no change: peer_id={peer_id} already at tier={tier} (idempotent)");
        return Ok(());
    }
    let prior_tier = prior.tier;
    let updated = airc_trust::set_tier(home, peer_id, tier)
        .await?
        .ok_or_else(|| {
            format!(
                "internal: peer {peer_id} disappeared between load and set_tier — \
             likely a concurrent `peer remove`; retry or check the trust store"
            )
        })?;
    println!(
        "tier_changed: peer_id={peer_id} {prior_tier} → {new}",
        new = updated.tier
    );

    // Best-effort daemon sync — same shape as run_peer_add /
    // run_peer_remove. The daemon currently has no SetTier RPC; on
    // a follow-up Sub-D it should subscribe to a TrustTierChanged
    // event and re-evaluate its in-memory verifier policy. For now
    // the trust store is the source of truth; the daemon will pick
    // up the new tier on its next read.
    let _ = socket; // placeholder until SetTier RPC ships (Sub-D)
    Ok(())
}

/// `peer list` — print enroled peers via `Airc::peers`. The daemon
/// writes the same trust store, so this view stays consistent
/// whether the daemon is running or not. `--json` produces the
/// machine-readable shape consumers (bridge, router) read off of.
pub async fn run_peer_list(home: &Path, json: bool) -> Result<(), Box<dyn std::error::Error>> {
    let peers = airc_trust::load(home).await?;
    if json {
        // Card 34942ec1 Sub-C V4: JSON shape is the contract
        // consumers read. Pin the field names + the tier wire
        // string so a future schema drift breaks the test in
        // peer_commands.rs, not the consumer at runtime.
        let rows: Vec<serde_json::Value> = peers
            .iter()
            .map(|p| {
                serde_json::json!({
                    "peer_id": p.peer_id.to_string(),
                    "pubkey_b64": p.pubkey_b64,
                    "added_at_ms": p.added_at_ms,
                    "tier": p.tier.as_wire_str(),
                    // Card 625abe6d slice 1: raw endpoint JSON (already
                    // a serde document; nesting it re-parsed keeps the
                    // machine surface honest about decode failures).
                    "endpoints_json": p.endpoints_json,
                    // Machine-vs-scope: the transport host whose TLS
                    // cert answers at the endpoints (dials pin THIS
                    // identity). Null = the peer answers itself.
                    "endpoints_peer_id": p.endpoints_peer_id.map(|host| host.to_string()),
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    for line in render_peer_list_lines(&peers, home) {
        println!("{line}");
    }
    Ok(())
}

/// Pure human-render of the peer trust store — the lines `peer list`
/// (and, via seam #2, `collaboration peers`) print. Extracted from
/// [`run_peer_list`] so the output **shape** is a pinnable contract:
/// the tier-aware line format and the trust-store-not-files source are
/// asserted by unit tests, rather than only surfacing as a downstream
/// script break. Reads exactly the [`airc_trust::load`] view — never a
/// `<home>/peers/*.json` file — which is the entire point of seam #2.
fn render_peer_list_lines(peers: &[airc_trust::StoredPeer], home: &Path) -> Vec<String> {
    if peers.is_empty() {
        return vec!["(no enroled peers — use `airc peer add <spec>` to enrol)".to_string()];
    }
    let mut lines = Vec::with_capacity(peers.len() + 2);
    for peer in peers {
        // Card 625abe6d slice 1: surface stored endpoints so the
        // operator can see what route discovery will dial. A record
        // with endpoint JSON this binary can't decode prints the
        // error inline rather than hiding the column.
        let endpoints = match peer.endpoints_json.as_deref() {
            None => String::new(),
            Some(json) => match airc_lib::endpoints_from_json(json) {
                Ok(endpoints) => format!("  endpoints={endpoints:?}"),
                Err(error) => format!("  endpoints=<undecodable: {error}>"),
            },
        };
        // Machine-vs-scope: show which identity actually answers at the
        // stored endpoints, so the mapping the dialer pins is visible.
        let host = match peer.endpoints_peer_id {
            Some(host) => format!("  host={host}"),
            None => String::new(),
        };
        lines.push(format!(
            "{}  {}  tier={}{endpoints}{host}",
            peer.peer_id,
            peer.pubkey_b64,
            peer.tier.as_wire_str()
        ));
    }
    lines.push(String::new());
    lines.push(format!(
        "{} peer(s) enroled at {}",
        peers.len(),
        home.display()
    ));
    lines
}

/// `whois <peer>` — print the trust entry for an enrolled peer.
///
/// Rich peer identity cards are a roster-layer follow-up. This command
/// is intentionally honest today: it resolves the peer trust entry that
/// controls message verification instead of pretending to have profile
/// metadata that is not yet published on the substrate.
pub async fn run_whois_peer(home: &Path, target: &str) -> Result<(), Box<dyn std::error::Error>> {
    let airc = Airc::open(home).await?;
    let peers = airc.peers().await?;
    let matches = peers
        .iter()
        .filter(|peer| peer.peer_id.to_string().starts_with(target))
        .collect::<Vec<_>>();

    match matches.as_slice() {
        [] => {
            println!("peer not found: {target}");
            if peers.is_empty() {
                println!("(no enroled peers — use `airc peer add <spec>` to enrol)");
            } else {
                println!("known peers:");
                for peer in peers {
                    println!("  {}  {}", peer.peer_id, peer.pubkey_b64);
                }
            }
            Err("peer not found".into())
        }
        [peer] => {
            println!("  peer_id:   {}", peer.peer_id);
            println!("  pubkey:    {}", peer.pubkey_b64);
            // Machine↔scope, one card (self-healing join): show the
            // transport HOST whose TLS cert answers at this peer's
            // endpoints, and — when this peer IS a host — the scope
            // peers reachable through it. Cert identity, joinable with
            // the registry machine-id in the identity card below. Read
            // from the same trust stores the dialer consults (wire root
            // first, then the scope store — `peers()` is the identity
            // union and doesn't carry endpoint metadata).
            let mut trust_records = airc_trust::load(airc.wire_root()).await.unwrap_or_default();
            if airc.wire_root() != home {
                trust_records.extend(airc_trust::load(home).await.unwrap_or_default());
            }
            let host_of = |target: airc_core::PeerId| {
                trust_records
                    .iter()
                    .find(|record| record.peer_id == target)
                    .and_then(|record| record.endpoints_peer_id)
                    .filter(|host| *host != target)
            };
            if let Some(host) = host_of(peer.peer_id) {
                println!(
                    "  machine:   {host} (transport host — TLS at this peer's \
                     endpoints answers as this identity; dials pin it)"
                );
            }
            let mut hosted: Vec<airc_core::PeerId> = trust_records
                .iter()
                .filter(|record| {
                    record.endpoints_peer_id == Some(peer.peer_id) && record.peer_id != peer.peer_id
                })
                .map(|record| record.peer_id)
                .collect();
            hosted.sort_by_key(|scope| scope.to_string());
            hosted.dedup();
            if !hosted.is_empty() {
                println!(
                    "  hosts:     {} scope peer(s) behind this machine:",
                    hosted.len()
                );
                for scope in hosted {
                    println!("             {scope}");
                }
            }
            // Card 20066c49: read the identity card the peer published
            // via the substrate (IdentityPublished events emitted on
            // join — cards 088af06 / cd638b8) when known. Falls back
            // to the honest "not published yet" line so the user can
            // tell unknown from blank-but-known.
            match airc.peer_identity_card(peer.peer_id).await {
                Ok(Some(card)) => {
                    let id = &card.identity;
                    let name = if id.name.is_empty() {
                        "(unset)"
                    } else {
                        id.name.as_str()
                    };
                    let pronouns = if id.pronouns.is_empty() {
                        "(unset)"
                    } else {
                        id.pronouns.as_str()
                    };
                    let role = if id.role.is_empty() {
                        "(unset)"
                    } else {
                        id.role.as_str()
                    };
                    let bio = if id.bio.is_empty() {
                        "(unset)"
                    } else {
                        id.bio.as_str()
                    };
                    let status = if id.status.is_empty() {
                        "(none)"
                    } else {
                        id.status.as_str()
                    };
                    let fingerprint = if id.fingerprint.is_empty() {
                        "(unset)"
                    } else {
                        id.fingerprint.as_str()
                    };
                    println!("  identity:  published");
                    println!("    name:        {name}");
                    println!("    pronouns:    {pronouns}");
                    println!("    role:        {role}");
                    println!("    bio:         {bio}");
                    println!("    status:      {status}");
                    println!("    fingerprint: {fingerprint}");
                    if !id.integrations.is_empty() {
                        println!("    integrations:");
                        for (k, v) in &id.integrations {
                            println!("      {k}: {v}");
                        }
                    }
                    println!("    emitted_at:  {} ms", card.emitted_at_ms);
                }
                Ok(None) => println!("  identity:  not published yet"),
                Err(error) => println!("  identity:  lookup failed: {error}"),
            }
            println!("  source:    peer trust store");
            Ok(())
        }
        _ => {
            println!("ambiguous peer prefix: {target}");
            for peer in matches {
                println!("  {}  {}", peer.peer_id, peer.pubkey_b64);
            }
            Err("ambiguous peer prefix".into())
        }
    }
}

// Silence the unused-import warning for `ClientId`: it's used
// transitively through `LocalIdentity::client_id` (the
// `airc_core::ClientId` newtype) but not referenced by name in this
// file. Keeping the import explicit makes the dep graph readable.
#[allow(dead_code)]
fn _client_id_kept_in_scope(_: ClientId) {}

fn runtime_headers() -> Result<Headers, Box<dyn std::error::Error>> {
    let mut headers = Headers::new();
    if let Some(client) = crate::client_id::current_client_id()? {
        headers.insert(HEADER_AIRC_CLIENT.to_string(), client);
    }
    Ok(headers)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// what this catches (live 2026-08-12): the @mention parse feeding the
    /// deaf-room warning. `@name` at the start of a body is an addressing
    /// intent and must lift the bare name (stopping at the first
    /// non-name char); a mid-text `@`, a bare `@`, or plain prose must
    /// not — a false mention would fire the audience warning on
    /// messages that never addressed anyone.
    #[test]
    fn leading_mention_lifts_addressing_intent_only() {
        assert_eq!(
            leading_mention("@BigMama LANE SPLIT per Joel"),
            Some("BigMama")
        );
        assert_eq!(
            leading_mention("@peer-7711fe60: ping"),
            Some("peer-7711fe60")
        );
        assert_eq!(leading_mention("@M5,ack"), Some("M5"));
        assert_eq!(leading_mention("hello @BigMama"), None);
        assert_eq!(leading_mention("@ stray at"), None);
        assert_eq!(leading_mention("plain prose"), None);
        assert_eq!(leading_mention(""), None);
    }

    fn roster_member(name: Option<&str>) -> airc_lib::RoomMember {
        airc_lib::RoomMember {
            peer_id: airc_lib::PeerId(uuid::Uuid::new_v4()),
            display_name: name.map(str::to_string),
            runtime: "test".to_string(),
            availability: None,
            last_seen_ms: 0,
        }
    }

    /// what this catches (live 2026-09-04, #262 follow-up): asserting ABSENCE
    /// about a peer the roster structurally cannot resolve. Both other grid
    /// peers were told "@them will NEVER receive this" seconds after each had
    /// posted into the room, because ONE other member — the sender's own
    /// published card — satisfied the old `any(display_name.is_some())` guard.
    /// `display_name: None` means present-but-unnamed, so an unmatched mention
    /// may BE that peer; only an all-named roster can license the strong line.
    ///
    /// Mutation check: restoring `any` in place of the unnamed check fails the
    /// mixed-roster assert.
    #[test]
    fn absence_is_only_asserted_when_every_present_peer_is_named() {
        let strong = "will NEVER receive";
        let me = roster_member(Some("IntelMac"));

        // Mixed roster — the live shape. One named member must NOT license a
        // negative about the unnamed ones.
        let mixed = vec![me.clone(), roster_member(None), roster_member(None)];
        let warning =
            mention_audience_verdict("M5", &mixed, me.peer_id, "academy").expect("unresolvable");
        assert!(!warning.contains(strong), "asserted absence: {warning}");

        // Empty roster — nobody seen at all cannot license it either.
        let empty =
            mention_audience_verdict("M5", &[], me.peer_id, "academy").expect("unresolvable");
        assert!(!empty.contains(strong), "asserted absence: {empty}");

        // All named and no match — the roster CAN answer, so the loud line is
        // earned. This is the #270 case the warning exists for.
        let named = vec![me.clone(), roster_member(Some("M5"))];
        let deaf =
            mention_audience_verdict("BigMama", &named, me.peer_id, "academy").expect("absent");
        assert!(deaf.contains(strong), "lost the real warning: {deaf}");
        // Both soft paths and the strong one carry the remediation that ended
        // #270 — it is true regardless of anyone's naming state.
        for line in [&warning, &empty, &deaf] {
            assert!(line.contains("--room general"), "lost remediation: {line}");
        }

        // A present, named match stays silent, as does a peer-id prefix.
        assert!(mention_audience_verdict("m5", &named, me.peer_id, "academy").is_none());
        let by_id = vec![roster_member(None)];
        let prefix = by_id[0].peer_id.to_string()[..8].to_uppercase();
        assert!(mention_audience_verdict(&prefix, &by_id, by_id[0].peer_id, "academy").is_none());
    }

    /// what this catches (#1378 review, card 74e8e6af): two defects in the
    /// `all` guard itself, both of which put a number or a verdict in front of
    /// the operator that the roster does not support.
    ///
    /// 1. A roster INCLUDES self. An operator who never ran `airc identity set`
    ///    is an unnamed member of every room, so counting self would make
    ///    `unnamed > 0` permanently true — the #270 warning could never fire
    ///    again for exactly the fresh-install operator most likely to need it,
    ///    and the soft line would describe them to themselves as a third party.
    /// 2. `room_roster_in` yields one row per (peer, CLIENT SESSION), so two
    ///    agent tabs on one box are two rows sharing a peer id. Counting rows
    ///    and printing "peer(s)" is a fabricated number.
    ///
    /// Mutation check: counting `roster` instead of `others` fails the first
    /// assert; counting rows instead of distinct peer ids fails the second.
    #[test]
    fn the_count_is_distinct_peers_and_never_includes_the_sender() {
        let me = roster_member(None); // uncarded operator — the fresh-install shape

        // Self must not suppress the real warning, even unnamed.
        let named_other = vec![me.clone(), roster_member(Some("M5"))];
        let deaf = mention_audience_verdict("BigMama", &named_other, me.peer_id, "academy")
            .expect("absent");
        assert!(
            deaf.contains("will NEVER receive"),
            "self suppressed the #270 warning: {deaf}"
        );

        // Two sessions of ONE unnamed peer is one peer, not two — and the
        // named peer plus that one makes two, not three.
        let twice = roster_member(None);
        let mut second_session = twice.clone();
        second_session.runtime = "codex".to_string();
        let rows = vec![
            me.clone(),
            twice.clone(),
            second_session,
            roster_member(Some("M5")),
        ];
        let soft = mention_audience_verdict("BigMama", &rows, me.peer_id, "academy")
            .expect("unresolvable");
        assert!(
            soft.contains("1 of 2 peer(s)"),
            "counted rows, not peers: {soft}"
        );

        // A roster of only the sender cannot license anything.
        let alone = vec![me.clone()];
        let solo =
            mention_audience_verdict("M5", &alone, me.peer_id, "academy").expect("unresolvable");
        assert!(
            solo.contains("no peer other than yourself"),
            "counted self as an audience: {solo}"
        );
    }

    /// what this catches (#267): relay self-election must try the PERSISTED
    /// previous port before an OS-assigned one — port drift across daemon
    /// restarts is what stranded every peer's imported relay card on a dead
    /// port. A persisted 0 (or nothing persisted) must not produce a
    /// degenerate double-\[0\] list.
    #[test]
    fn relay_bind_prefers_the_persisted_port_with_os_fallback() {
        assert_eq!(relay_bind_candidates(Some(65280)), vec![65280, 0]);
        assert_eq!(relay_bind_candidates(None), vec![0]);
        assert_eq!(relay_bind_candidates(Some(0)), vec![0]);
    }

    /// what this catches: the send receipt must NOT report the enrolled
    /// peer count as confirmed delivery. The old line was
    /// "sent to X — N paired peer(s) + any local scope tailing …",
    /// which made a send look successful even when zero peers actually
    /// received anything (the false "it works" bug). The receipt is
    /// honest: the verb is "queued"/"addressed" (not "sent to N peers"),
    /// it never claims "paired peer(s)" delivered, and it tells the
    /// operator delivery is asynchronous + how to confirm it.
    #[test]
    fn send_receipt_does_not_imply_confirmed_delivery() {
        // 41 enrolled, all currently connected → the healthy branch.
        let line = format_send_receipt("general", "cb2e21a1", 41, 41);

        // The exact misleading phrasing is gone.
        assert!(
            !line.contains("41 paired peer(s)"),
            "must not report enrolled count as delivery: {line}"
        );
        assert!(
            !line.contains("paired peer"),
            "the enrolled-peer count must not masquerade as delivery: {line}"
        );
        // It must not lead with the delivery-implying verb "sent to".
        assert!(
            !line.starts_with("sent to"),
            "verb must not imply confirmed delivery: {line}"
        );
        // It IS honest about what happened and what is unconfirmed.
        assert!(line.contains("queued to general"), "honest verb: {line}");
        assert!(
            line.contains("enrolled"),
            "the address-book count is still disclosed, just not as reach: {line}"
        );
        assert!(
            line.contains("asynchronous"),
            "must state delivery is unconfirmed: {line}"
        );
        assert!(
            line.contains("airc doctor --health"),
            "must tell operator how to confirm delivery: {line}"
        );
    }

    /// what this catches (#351): the receipt inviting the reader to compute a
    /// REACH RATIO out of two incommensurable numbers.
    ///
    /// It used to print "addressed 52 enrolled remote peer(s), 1 currently
    /// connected". Enrolled is every peer this scope ever met, across every room,
    /// for all time — not this room's audience — and the live count is LAN links
    /// only. Read as a fraction it says 2%. The ack ledger for the same period
    /// said 767 of 771 delivered: 99.5%. A card (#340) was filed on the strength
    /// of that line, and the line was the only thing wrong.
    ///
    /// An instrument that reads as catastrophe during healthy operation is worse
    /// than no instrument — it spends the trust that a REAL alarm will need.
    #[test]
    fn the_receipt_never_reads_as_a_reach_ratio() {
        // The shape that caused the false alarm: a big address book, one live link.
        let line = format_send_receipt("cambriantech", "cb2e21a1", 52, 1);

        // The adjacency itself was the bug — "N enrolled …, M currently connected"
        // sitting together is what the eye divides. Neither the old phrasing nor
        // the old ordering may come back.
        assert!(
            !line.contains("currently connected"),
            "a live-link count must not sit where it reads as the numerator: {line}"
        );
        assert!(
            !line.contains("addressed 52"),
            "the address book is not an audience and must not be 'addressed': {line}"
        );
        // It must say so outright, because a reader who has been burned once will
        // keep dividing unless told the denominator is not a denominator.
        assert!(
            line.contains("not this room's audience"),
            "must disclaim the address book explicitly: {line}"
        );
        // And it must point at the ACK ledger, which is what delivery actually
        // means (#280: delivery is a returned ack, never a connection's existence).
        assert!(
            line.contains("ACK") || line.contains("ack"),
            "delivery truth is the ack ledger, not this line: {line}"
        );
    }

    /// what this catches: the zero-enrolled-peer case stays honest —
    /// no false "delivered" claim, but it correctly preserves the
    /// load-bearing truth that same-machine tailers still receive the
    /// frame (the lone-node / two-scopes-one-home topology). Count is
    /// reported as enrolled, never as a delivery confirmation.
    #[test]
    fn send_receipt_zero_peers_is_honest_about_local_tailers() {
        let line = format_send_receipt("general", "cb2e21a1", 0, 0);
        assert!(!line.starts_with("sent to"), "no delivery verb: {line}");
        assert!(
            !line.contains("paired peer"),
            "no paired-peer claim: {line}"
        );
        assert!(line.contains("queued to general"), "honest verb: {line}");
        assert!(
            line.contains("0 enrolled remote peer(s)"),
            "honest about zero enrolled peers: {line}"
        );
        assert!(
            line.contains("tailing this channel on this machine"),
            "must preserve the same-machine-delivery truth: {line}"
        );
    }

    /// what this catches (issue #1243 — the legibility trap): enrolled
    /// peers exist but the daemon holds ZERO live LAN connections, so
    /// room broadcast forwarded to nobody. The receipt must say so
    /// LOUDLY (not report the enrolled "address book" count as if it
    /// were reach) — the silent-success of this exact case let a fully
    /// broken fan-out masquerade as a healthy channel for an hour.
    #[test]
    fn send_receipt_warns_loudly_when_no_peer_is_connected() {
        let line = format_send_receipt("general", "cb2e21a1", 42, 0);
        assert!(line.contains("queued to general"), "honest verb: {line}");
        // It must NOT frame 42 enrolled as if 42 received it.
        assert!(
            line.contains("reached 0 of 42"),
            "must surface that 0 of 42 enrolled peers were reached: {line}"
        );
        assert!(
            line.contains("NONE are currently connected"),
            "must name the broken-route cause, not imply delivery: {line}"
        );
        assert!(
            line.contains("airc doctor --health"),
            "must point the operator at the route diagnostic: {line}"
        );
        // The same-machine truth is preserved even when remotes are dark.
        assert!(
            line.contains("tailing this channel on this machine"),
            "local tailers still receive it: {line}"
        );
    }

    /// what this catches: the healthy branch reports BOTH counts —
    /// enrolled (address book) and the live-connected subset — without
    /// claiming confirmed delivery. Distinguishes "addressed N, M
    /// connected" from the loud zero-connected case above.
    #[test]
    fn send_receipt_reports_connected_subset_when_routes_are_up() {
        let line = format_send_receipt("general", "cb2e21a1", 42, 3);
        // Both facts are still disclosed — the fix was never about hiding a
        // number, it was about not pairing them as numerator and denominator.
        assert!(
            line.contains("3 peers"),
            "the live-link count is still reported: {line}"
        );
        assert!(
            line.contains("42 peer(s) are enrolled"),
            "the address-book size is still reported, as its own fact: {line}"
        );
        assert!(
            line.contains("asynchronous"),
            "still does not claim confirmed delivery: {line}"
        );
    }

    /// what this catches (seam #2): `collaboration peers` / `peer list`
    /// render the canonical trust store with ALL tiers visible — an
    /// Untrusted and a Friend peer both appear, each with its `tier=`
    /// shape. Pins the output contract so a tier-filter regression or a
    /// line-shape change breaks here, not a downstream script.
    #[tokio::test]
    async fn render_peer_list_shows_every_tier_from_trust_store() {
        let home = tempfile::tempdir().expect("home");
        let untrusted = PeerId::from_u128(0xa1);
        let friend = PeerId::from_u128(0xf2);
        airc_trust::add(home.path(), untrusted, [0xAA; 32])
            .await
            .expect("add untrusted");
        airc_trust::add(home.path(), friend, [0xBB; 32])
            .await
            .expect("add friend");
        airc_trust::set_tier(home.path(), friend, airc_store::TrustTier::Friend)
            .await
            .expect("set tier")
            .expect("friend enrolled");

        let peers = airc_trust::load(home.path()).await.expect("load");
        let lines = render_peer_list_lines(&peers, home.path()).join("\n");

        assert!(
            lines.contains(&untrusted.to_string()),
            "untrusted peer must render: {lines}"
        );
        assert!(
            lines.contains(&friend.to_string()),
            "friend peer must render: {lines}"
        );
        assert!(
            lines.contains("tier=untrusted"),
            "tier shape pinned: {lines}"
        );
        assert!(lines.contains("tier=friend"), "tier shape pinned: {lines}");
        assert!(
            lines.contains("2 peer(s) enroled"),
            "summary count from trust store: {lines}"
        );
    }

    /// what this catches (seam #2, the whole point): the render reads
    /// the trust store, NEVER a legacy `<home>/peers/*.json` file. A
    /// stray legacy record dropped in the same home must be invisible —
    /// otherwise the two-peer-systems drift seam #2 closes is still open.
    #[tokio::test]
    async fn render_peer_list_ignores_legacy_peers_json_files() {
        let home = tempfile::tempdir().expect("home");
        let real = PeerId::from_u128(0x7e57);
        airc_trust::add(home.path(), real, [0xCC; 32])
            .await
            .expect("add real");

        // Legacy file-based record in the same home — the surface the
        // old `collaboration peers` rendered. Must NOT leak into the view.
        let peers_dir = home.path().join("peers");
        std::fs::create_dir_all(&peers_dir).expect("peers dir");
        std::fs::write(
            peers_dir.join("ghost.json"),
            r#"{"name":"ghost","host":"ghost@host","paired":"2026-01-01T00:00:00Z"}"#,
        )
        .expect("write ghost");

        let peers = airc_trust::load(home.path()).await.expect("load");
        let lines = render_peer_list_lines(&peers, home.path()).join("\n");

        assert!(
            lines.contains(&real.to_string()),
            "trust-store peer must render: {lines}"
        );
        assert!(
            !lines.contains("ghost"),
            "legacy peers/*.json must NOT appear in the trust-store view: {lines}"
        );
        assert!(
            lines.contains("1 peer(s) enroled"),
            "count reflects the trust store only, not the legacy file: {lines}"
        );
    }

    /// what this catches: the peer-prune staleness override math.
    /// `None` must resolve to the substrate default (not 0 — which would
    /// silently disable the grace window); an explicit hour count must
    /// convert to ms; `0` must mean no grace; and an absurd value must
    /// saturate rather than overflow-panic.
    #[test]
    fn resolve_stale_after_ms_maps_override_hours() {
        assert_eq!(
            resolve_stale_after_ms(None),
            airc_lib::DEFAULT_PEER_STALE_AFTER_MS,
            "omitting the flag uses the substrate default"
        );
        assert_eq!(resolve_stale_after_ms(Some(2)), 7_200_000, "2h → ms");
        assert_eq!(
            resolve_stale_after_ms(Some(0)),
            0,
            "0 hours = no grace (evict every absent untrusted peer)"
        );
        assert_eq!(
            resolve_stale_after_ms(Some(u64::MAX)),
            u64::MAX,
            "absurd hour count saturates, never overflow-panics"
        );
    }

    /// what this catches: the empty-store render is the honest
    /// onboarding hint, not a blank screen or a panic on no peers.
    #[test]
    fn render_peer_list_empty_is_onboarding_hint() {
        let home = std::path::Path::new("/tmp/airc-empty");
        let lines = render_peer_list_lines(&[], home);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("no enroled peers"), "{:?}", lines);
        assert!(lines[0].contains("airc peer add"), "{:?}", lines);
    }

    /// Card bf7c30e2: the preflight error is self-diagnosing — it
    /// names the stores consulted (scope + machine), the union count,
    /// and the scoped-store cause, so a cwd mismatch is identifiable
    /// from the error alone. Hermetic: isolated wire root, so the
    /// union cannot see the real machine store.
    #[tokio::test]
    async fn lan_send_preflight_names_the_stores_it_consulted() {
        let scope = tempfile::tempdir().expect("scope");
        let wire_root = tempfile::tempdir().expect("wire root");
        let airc = Airc::open_with_wire_root_for_test(
            scope.path().to_path_buf(),
            wire_root.path().to_path_buf(),
        )
        .await
        .expect("open");
        let expected = PeerId::from_uuid("55536f5f-ffde-4e9f-ae1f-32d1a33ec31e".parse().unwrap());
        let err = preflight_expected_peer(&airc, scope.path(), &[], expected)
            .await
            .expect_err("unknown peer must fail preflight");
        let msg = err.to_string();
        assert!(
            msg.contains(&scope.path().display().to_string()),
            "error must name the scope store path: {msg}"
        );
        assert!(
            msg.contains("machine store"),
            "error must name the machine store: {msg}"
        );
        assert!(
            msg.contains("peer(s)"),
            "error must state the union count: {msg}"
        );
    }

    /// An ad-hoc `--peer` spec for the expected peer satisfies the
    /// preflight without touching any persistent store; and a peer
    /// enrolled in the persistent union passes (preflight sources ==
    /// verifier sources — the round-1 review catch).
    #[tokio::test]
    async fn lan_send_preflight_accepts_volatile_and_union_peers() {
        let scope = tempfile::tempdir().expect("scope");
        let wire_root = tempfile::tempdir().expect("wire root");
        let airc = Airc::open_with_wire_root_for_test(
            scope.path().to_path_buf(),
            wire_root.path().to_path_buf(),
        )
        .await
        .expect("open");
        let spec: PeerSpec =
            "55536f5f-ffde-4e9f-ae1f-32d1a33ec31e:-OPD_KbcJrqfZlXcBiN9x3QN9EtahW4URXCdY30b-s8"
                .parse()
                .expect("spec parses");
        let expected = spec.peer_id;
        preflight_expected_peer(&airc, scope.path(), std::slice::from_ref(&spec), expected)
            .await
            .expect("volatile spec must satisfy preflight");

        // THE DISCRIMINATING CASE (round-3 mutation-test catch): enrol
        // into the WIRE-ROOT (machine) store ONLY — the store round-1's
        // buggy preflight could not see. This test fails under the
        // round-1 mutation (`airc_trust::load(home)`) and passes with
        // the union; the prior version (add_peer → scope store) passed
        // under both and pinned nothing.
        peers_store::add(wire_root.path(), spec.peer_id, spec.pubkey)
            .await
            .expect("enrol into wire-root store");
        preflight_expected_peer(&airc, scope.path(), &[], expected)
            .await
            .expect("machine-store-only peer must satisfy preflight (verifier union)");

        // Self-dial: peers() filters self, the verifier accepts self —
        // preflight must side with the verifier.
        preflight_expected_peer(&airc, scope.path(), &[], airc.peer_id())
            .await
            .expect("own peer id must always pass preflight");
    }

    fn status(commit: Option<&str>, protocol: Option<u32>) -> airc_ipc::StatusResponse {
        airc_ipc::StatusResponse {
            peer_id: "07e7ad58-ba56-4535-b4e5-a161a110e487".to_string(),
            uptime_seconds: 1,
            ipc_protocol_version: protocol,
            build_commit: commit.map(str::to_string),
            build_branch: Some("rust-rewrite".to_string()),
            executable: Some("/tmp/airc".to_string()),
            connected_lan_peers: 0,
        }
    }

    #[test]
    fn daemon_status_current_requires_matching_protocol_and_build() {
        assert!(daemon_status_is_current(&status(
            Some(crate::build_info::COMMIT),
            Some(u32::from(airc_ipc::IPC_PROTOCOL_VERSION))
        )));
        assert!(!daemon_status_is_current(&status(
            Some("old-build"),
            Some(u32::from(airc_ipc::IPC_PROTOCOL_VERSION))
        )));
        assert!(!daemon_status_is_current(&status(
            Some(crate::build_info::COMMIT),
            Some(u32::from(airc_ipc::IPC_PROTOCOL_VERSION) + 1)
        )));
    }

    #[test]
    fn daemon_status_without_metadata_is_stale() {
        assert!(!daemon_status_is_current(&status(None, None)));
    }

    // Card 1f2cbffa item 3 (#1145 audit): AIRC_GH_BIN is the
    // operator's authoritative gh override. `resolve_gh_bin` used to
    // ignore it entirely, and `run_daemon` then clobbered the store's
    // env-honoring default via `.with_bin(...)`. These pins drive the
    // env-free `_with` body so no racy process-env mutation is needed.
    #[test]
    fn resolve_gh_bin_honors_airc_gh_bin_override() {
        let dir = tempfile::tempdir().expect("tempdir");
        let custom = dir.path().join("custom-gh");
        std::fs::write(&custom, "").expect("write stub");
        assert_eq!(
            resolve_gh_bin_with(Some(custom.clone())),
            Some(custom),
            "an existing AIRC_GH_BIN must be returned verbatim, not a PATH-resolved gh"
        );
    }

    // A broken override must surface loudly downstream (every gh
    // spawn fails per tick), NEVER silently swap to a PATH gh the
    // operator deliberately overrode away from. Mutation check:
    // re-adding a PATH fallback for a missing override makes this
    // return a real gh on any gh-installed machine and the assert
    // fails.
    #[test]
    fn resolve_gh_bin_broken_override_never_falls_back_to_path() {
        let missing = std::path::PathBuf::from("/nonexistent/airc-gh-override-pin-1f2cbffa/gh");
        assert_eq!(
            resolve_gh_bin_with(Some(missing.clone())),
            Some(missing),
            "a missing AIRC_GH_BIN must still be honored (loud failure downstream), \
             never silently replaced by a PATH-resolved gh"
        );
    }

    // Token extraction goes through the SAME override: with
    // AIRC_GH_BIN set, only that binary is consulted.
    #[cfg(unix)]
    #[test]
    fn resolve_gh_token_uses_the_override_binary_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let stub = dir.path().join("gh");
        std::fs::write(&stub, "#!/bin/sh\nprintf 'override-token\\n'\nexit 0\n")
            .expect("write stub");
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755))
            .expect("chmod stub");
        assert_eq!(
            resolve_gh_token_with(Some(stub)).as_deref(),
            Some("override-token")
        );
        // Broken override: no silent fallback to a PATH gh's token.
        assert_eq!(
            resolve_gh_token_with(Some(std::path::PathBuf::from(
                "/nonexistent/airc-gh-override-pin-1f2cbffa/gh"
            ))),
            None,
            "a broken AIRC_GH_BIN must yield no token, never a PATH gh's token"
        );
    }
}
