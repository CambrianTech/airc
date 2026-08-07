use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

pub fn run_update(home: &Path, socket: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let source = install_source_dir()?;
    validate_source_checkout(&source)?;
    let airc_exe = env::current_exe()?;
    let daemon_was_running = daemon_is_running(&airc_exe, home, &socket)?;

    let channel = update_channel();

    // ── The blackout window is the product, not a side effect ──────────────
    //
    // Incident 2026-08-08: a peer's daemon was down ~4 minutes during an
    // `airc update`, and every message sent to that node in the window was
    // LOST — not queued, not replayed. Replay resumes from the RECEIVER's
    // cursor against the RECEIVER's store, so a frame that never reached that
    // store cannot be recovered; the receiver cannot even know it missed one.
    // Generalized: every update costs the grid a coordination blackout on each
    // node as it rolls, and the traffic most likely to be in flight during an
    // update is the traffic coordinating it.
    //
    // Durable store-and-forward (#47) is the other half and is not this fix.
    // This one shrinks the window that makes #47 fire on a SCHEDULE rather
    // than by accident, by taking the daemon down only for what actually
    // requires it — replacing the binary — instead of for the git fetch and
    // the multi-minute rebuild that preceded it.
    //
    // Ordering below, in the order the reasons apply:
    //   1. fetch/checkout — a git op on a separate worktree, no daemon involved
    //   2. nothing-to-do  — return WITHOUT ever stopping the daemon
    //   3. pre-warm build — compile while the node is still on the air
    //   4. stop → install → restart — the genuinely exclusive part
    let (build_dir, before, after) = prepare_build_source(&source, &channel)?;

    // NOTHING TO DO → NO BLACKOUT.
    //
    // This condition already existed and decided only a println: the updater
    // learned the source had not moved, then stopped the daemon, ran the full
    // installer, and restarted anyway — so a node that was already current
    // paid the entire outage for a no-op, on every check. Auto-update runs on
    // a cadence, so those were recurring outages bought with nothing.
    //
    // Both halves are required. `before == after` says the SOURCE did not move
    // this run; it does NOT say the installed binary matches it (#354: a prior
    // install can have failed while the checkout stayed current). The smoke
    // test is what makes skipping safe — unchanged source AND a binary that
    // already reports it means there is genuinely nothing to do.
    if before == after && smoke_test_new_binary(&airc_exe, &after) {
        println!("Already at {after} on channel {channel} — daemon left running.");
        return Ok(());
    }

    // Compile BEFORE the node goes off the air. `install.sh` runs
    // `cargo build --release -p airc-cli` in this same directory, so this warms
    // exactly the cache it will use and its rebuild becomes near-incremental —
    // the outage shrinks from "fetch + full rebuild + install" to roughly
    // "install + restart".
    //
    // Best-effort ON PURPOSE, and not a masking fallback: `run_installer` below
    // performs the authoritative build moments later and fails loud if the code
    // does not compile. Nothing is hidden by ignoring a failure here — a broken
    // build still stops the update, it just stops it a few seconds later.
    prewarm_build(&build_dir);

    if daemon_was_running {
        stop_daemon(&airc_exe, home, &socket)?;
    }
    // What the OPERATOR is holding, read before we replace it. `before`/`after`
    // above describe the git checkout; this describes the tool. They are
    // independent, and the summary at the end has to speak about this one.
    // (Canary's #1332 moved `prepare_build_source` before the no-op gate;
    // this read only has to precede `run_installer`, which is what replaces
    // the binary.)
    let binary_before = installed_binary_sha(&airc_exe);

    run_installer(&build_dir)?;

    // Prove the BINARY became `after` before claiming anything about it (#354).
    //
    // Everything above this line is a statement about a git checkout; the
    // operator reads the lines below as statements about the tool they are
    // holding. Those were allowed to disagree silently — and did, on a live
    // peer node on 2026-08-07: `airc update` printed "Already at 1e2f424 …
    // daemon: restarted." and `airc --version` on the very next line said
    // *3 commits behind*. Both true, neither lying, describing different
    // objects.
    //
    // The check itself was never missing. `run_auto_update` has smoke-tested
    // since it was written (`smoke_test_new_binary`, with rollback). It was
    // simply never wired into the MANUAL path — the one the staleness banner
    // tells you to run, and therefore the one a human or an agent actually
    // reaches for. Built, correct, and not called where it mattered.
    //
    // Deliberately NOT mirroring the auto path's rollback here: this path is
    // entered on purpose by someone who can re-run it, and a rollback needs
    // its own backup anchor + failure modes. Verification is what was missing;
    // silently rolling back an operator's explicit action is a separate call.
    //
    // Nor does this SELF-HEAL, unlike the daemon check below — and the
    // asymmetry is the point, not an oversight. Heal what has a known-safe
    // idempotent remedy; report what needs a human decision. A stale daemon is
    // the former: stop it, start it, done. A binary that did not land is
    // usually the installer writing somewhere other than what the shell
    // resolves, and re-running an installer that already did its job cannot fix
    // a PATH. Retrying there would be theatre — it would burn minutes, change
    // nothing, and teach the operator that the check is noise.
    if !smoke_test_new_binary(&airc_exe, &after) {
        return Err(format!(
            "update did NOT take: the source reached {after}, but the binary at \
             {} does not report it. Nothing verified this before, so this printed \
             a success line instead. Check `which -a airc` — the installer may be \
             writing somewhere other than the path your shell resolves.",
            airc_exe.display()
        )
        .into());
    }

    // Report the BINARY's transition, not the checkout's (#354 follow-up).
    //
    // #354 made the update VERIFY the binary but left the summary branching on
    // `before == after` — two git refs. Those describe the checkout, and the
    // checkout can already be current while the binary is stale, so the two
    // most different outcomes printed the SAME line:
    //
    //   nothing happened                     -> "Already at a08f3d3 on channel canary."
    //   your binary was just replaced        -> "Already at a08f3d3 on channel canary."
    //
    // Measured on BigMama 2026-08-07, immediately after #354 landed: source was
    // already at a08f3d3 (a manual pull had moved it), the installed binary was
    // still 35d40b1, `airc update` rebuilt and installed a08f3d3 — and printed
    // "Already at". The verification worked; the sentence describing it did not.
    // An operator reading "Already at" reasonably concludes no work was done and
    // does not restart anything that embeds the binary.
    //
    // Same defect as #354, one layer down: #354 fixed which object we CHECK,
    // this fixes which object we TALK ABOUT. Both halves have to point at the
    // tool the operator is holding.
    println!(
        "{}",
        update_summary(binary_before.as_deref(), &before, &after, &channel)
    );
    if daemon_was_running {
        restart_daemon(&airc_exe, home, &socket)?;
        wait_daemon_ready(&airc_exe, home, &socket)?;
        // The NEXT link in the chain. `wait_daemon_ready` proves a daemon
        // answers; it does not prove it is the daemon we just built. A stale
        // process that survived `stop_daemon` answers IPC perfectly, so
        // "daemon: restarted." was true and useless — the exact shape
        // `check_daemon_build` in doctor.rs was written for after it went
        // undetected on a live node for hours. That check only runs under
        // `doctor --health`; update restarts the daemon and never asked.
        verify_daemon_build(&airc_exe, home, &socket, &after)?;
        println!("daemon: restarted (build verified).");
    }
    Ok(())
}

/// The release channel this node's updates track — DURABLE per-machine config,
/// never the install checkout's whim. Resolution: `AIRC_UPDATE_CHANNEL` env →
/// `~/.airc/update-channel` file → `"canary"` (the rust-rewrite release branch).
///
/// Why this exists (task #288, incident 2026-08-01): the updater used to build
/// whatever branch the install-source checkout happened to have checked out.
/// On a dev checkout that branch can be a feature branch or a DELETED PR branch
/// — updates then either brick ("couldn't find remote ref …") or silently
/// strand the node off-channel, which is exactly how a peer's daemon misses a
/// committed transport fix for a day. The channel is the node's contract; the
/// checkout is just where the objects live.
pub(crate) const DEFAULT_UPDATE_CHANNEL: &str = "canary";

fn update_channel() -> String {
    if let Some(ch) = env::var("AIRC_UPDATE_CHANNEL")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        return ch;
    }
    machine_airc_home()
        .and_then(|d| channel_from_file(&d.join("update-channel")))
        .unwrap_or_else(|| DEFAULT_UPDATE_CHANNEL.to_string())
}

/// Read a channel name from the durable file; `None` on missing/empty/unreadable
/// so the caller falls through to the default. Pure over the path — unit-tested.
fn channel_from_file(path: &Path) -> Option<String> {
    let contents = std::fs::read_to_string(path).ok()?;
    let trimmed = contents.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// The per-user `~/.airc` dir (same resolution as the gh guard state) — home for
/// the update channel file and the channel build worktree.
fn machine_airc_home() -> Option<PathBuf> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(|h| PathBuf::from(h).join(".airc"))
}

/// Prepare the directory the update BUILDS from, pinned to the release channel,
/// WITHOUT ever mutating a dev checkout's working tree. Returns
/// `(build_dir, before_short_sha, after_short_sha)`; `before == after` is the
/// caller's no-op signal.
///
/// - Checkout already ON the channel branch → fetch + ff-pull in place (the
///   pre-existing fast path, unchanged behavior for `~/.airc/src` installs).
/// - Checkout on ANY other branch, or detached (including a deleted PR branch —
///   the state that bricked `airc update` on 2026-08-01) → build from an
///   airc-owned worktree at `~/.airc/update-worktree` hard-reset to
///   `origin/<channel>`. The dev checkout is only ever `git fetch`ed; its
///   branch, index, and working tree are never touched. The worktree shares the
///   source repo's objects, so no re-clone.
fn prepare_build_source(
    source: &Path,
    channel: &str,
) -> Result<(PathBuf, String, String), Box<dyn std::error::Error>> {
    run_checked(
        Command::new("git")
            .arg("-C")
            .arg(source)
            .args(["fetch", "--quiet", "origin", channel]),
        "git fetch (channel)",
    )?;
    let branch = git_text(source, ["rev-parse", "--abbrev-ref", "HEAD"])?;
    if branch == channel {
        let before = git_text(source, ["rev-parse", "--short", "HEAD"])?;
        run_checked(
            Command::new("git")
                .arg("-C")
                .arg(source)
                .args(["pull", "--ff-only", "--quiet"]),
            "git pull --ff-only",
        )?;
        let after = git_text(source, ["rev-parse", "--short", "HEAD"])?;
        return Ok((source.to_path_buf(), before, after));
    }

    let wt = machine_airc_home()
        .ok_or("HOME is not set; cannot place the update worktree")?
        .join("update-worktree");
    let origin_ref = format!("origin/{channel}");
    if wt.join(".git").exists() {
        // Reuse: `before` is what this node last built from the channel, so the
        // auto path's no-op compare stays meaningful across runs.
        let before = git_text(&wt, ["rev-parse", "--short", "HEAD"]).unwrap_or_default();
        run_checked(
            Command::new("git")
                .arg("-C")
                .arg(&wt)
                .args(["reset", "--hard", &origin_ref]),
            "git reset --hard (update worktree)",
        )?;
        let after = git_text(&wt, ["rev-parse", "--short", "HEAD"])?;
        Ok((wt, before, after))
    } else {
        if wt.exists() {
            // Half-created leftover (no .git link) — clear it and drop any stale
            // registration so `worktree add` can't refuse.
            std::fs::remove_dir_all(&wt)?;
        }
        let _ = Command::new("git")
            .arg("-C")
            .arg(source)
            .args(["worktree", "prune"])
            .output();
        let wt_str = wt
            .to_str()
            .ok_or("update worktree path is not valid UTF-8")?
            .to_string();
        run_checked(
            Command::new("git").arg("-C").arg(source).args([
                "worktree",
                "add",
                "--detach",
                &wt_str,
                &origin_ref,
            ]),
            "git worktree add (update worktree)",
        )?;
        let after = git_text(&wt, ["rev-parse", "--short", "HEAD"])?;
        // Empty `before` ≠ `after` → first channel build always proceeds.
        Ok((wt, String::new(), after))
    }
}

/// `airc update --auto` — self-update with a smoke-test and rollback.
///
/// The safe sibling of [`run_update`]: it backs up the live binary,
/// rebuilds, runs the new binary through a smoke-test, and ROLLS BACK to
/// the backup if the new build is broken. So an auto-update can never
/// leave a peer with a binary that compiles-but-doesn't-run.
///
/// Flow: fetch + ff-pull the channel → if HEAD unchanged, nothing to do
/// (the daemon is NEVER touched — see below) → else stop the daemon, back
/// up the installed binary to `airc.prev`, rebuild in place, smoke-test
/// (the new binary's `version` reports the pulled SHA), and on failure
/// restore `airc.prev`.
///
/// The no-op path must not restart the daemon: `git fetch`/`pull` only
/// touch the source checkout, never the running binary, so only the
/// rebuild+swap needs the daemon down. The pre-fix shape stopped the
/// daemon BEFORE the SHA compare, which killed the transport owner every
/// hourly "nothing to auto-update" tick — wiping in-process room state
/// and blinding every subscribed client for the restart window
/// (continuum blind-room incidents #2/#3, 2026-07-11/12).
///
/// Platform note: on Windows the live `airc.exe` is locked while this
/// process runs, so the in-place reinstall (and thus the swap) inherits
/// the same constraint as `run_update` — most valuable on the macOS /
/// Linux grid nodes today. The rollback path is a no-op there because no
/// swap occurred.
pub fn run_update_auto(home: &Path, socket: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let source = install_source_dir()?;
    validate_source_checkout(&source)?;
    let airc_exe = env::current_exe()?;
    let daemon_was_running = daemon_is_running(&airc_exe, home, &socket)?;

    let channel = update_channel();
    // Channel-pinned build source: fetch/reset only ever touch the channel
    // worktree (or ff-pull a checkout that IS the channel) — never the running
    // binary and never a dev checkout's working tree.
    let (build_dir, before, after) = prepare_build_source(&source, &channel)?;

    if before == after {
        // Nothing pulled → nothing to rebuild → the daemon was never
        // stopped and MUST NOT be restarted. Restart-on-no-op is the bug
        // this ordering exists to prevent (hourly transport-owner death).
        println!("Already at {after} on channel {channel} — nothing to auto-update.");
        return Ok(());
    }

    // A real update is pending — only NOW does the binary swap need the
    // transport owner down.
    if daemon_was_running {
        stop_daemon(&airc_exe, home, &socket)?;
    }

    // Back up the live binary BEFORE the rebuild — this is the rollback
    // anchor. Copying a running exe for read is allowed on every platform.
    let prev = airc_exe.with_file_name("airc.prev");
    std::fs::copy(&airc_exe, &prev).map_err(|e| {
        format!(
            "could not back up the current binary to {}: {e}",
            prev.display()
        )
    })?;

    // Get the live exe OUT OF THE WAY before the installer writes.
    //
    // Windows refuses to overwrite a running executable (`Device or resource
    // busy` / ERROR_SHARING_VIOLATION) — and `stop_daemon` above does not clear
    // it, because other processes share this binary: other scopes' daemons, a
    // `join` stream, the ACP bridge. Measured on BigMama: two `airc.exe` plus
    // `airc-acp-bridge.exe` still holding it after a clean stop.
    //
    // So the installer's copy silently failed, the smoke-test then failed
    // (correctly — the exe still reported the OLD sha), and the whole thing
    // reported a ROLLBACK. `airc update` has never once updated a Windows box,
    // and it never said so: the rollback branch's own comment concedes the
    // reinstall "couldn't replace the locked live exe in the first place" and
    // treats that as fine.
    //
    // Windows DOES allow renaming a running exe — the handle follows the inode,
    // the process keeps running from the renamed file. That is the standard
    // self-update idiom on this platform. Move it aside and the installer writes
    // to a free path.
    //
    // Unix does not need this (write-over-running is legal there), but it is
    // harmless and one code path beats two.
    let displaced = airc_exe.with_file_name(format!("airc.old-{before}"));
    let _ = std::fs::remove_file(&displaced); // a previous update's leftover
    if let Err(e) = std::fs::rename(&airc_exe, &displaced) {
        return Err(format!(
            "could not move the live binary aside before installing ({e}). \
             {} is still the running executable and nothing was changed. \
             Something holds it that a rename cannot displace — check for \
             other airc processes ({}).",
            airc_exe.display(),
            "airc.exe, airc-acp-bridge.exe"
        )
        .into());
    }

    let installed = run_installer(&build_dir);

    // If the installer did not produce a binary, put the original back NOW —
    // otherwise the rename above has left the box with no `airc` on PATH at
    // all, which is strictly worse than a stale one.
    if installed.is_err() || !airc_exe.exists() {
        let _ = std::fs::rename(&displaced, &airc_exe);
    }

    // Smoke-test: the new binary must RUN and report the SHA we pulled —
    // a build that compiled but is broken (or didn't actually replace the
    // binary) fails here and triggers rollback.
    let smoke_ok = installed.is_ok() && smoke_test_new_binary(&airc_exe, &after);

    if smoke_ok {
        println!(
            "Auto-updated: {before} -> {after} (smoke-test passed; backup at {}).",
            prev.display()
        );
        if daemon_was_running {
            restart_daemon(&airc_exe, home, &socket)?;
            wait_daemon_ready(&airc_exe, home, &socket)?;
        }
        Ok(())
    } else {
        eprintln!("⚠ new build did not pass the smoke-test — ROLLING BACK to the previous binary.");
        // Restore the known-good binary. (No-op-safe on Windows where the
        // reinstall couldn't replace the locked live exe in the first place.)
        if let Err(e) = std::fs::copy(&prev, &airc_exe) {
            return Err(format!(
                "auto-update FAILED and rollback ALSO failed ({e}); \
                 your previous binary is at {} — restore it manually",
                prev.display()
            )
            .into());
        }
        if daemon_was_running {
            // Restart on the rolled-back (known-good) binary.
            let _ = restart_daemon(&airc_exe, home, &socket);
            let _ = wait_daemon_ready(&airc_exe, home, &socket);
        }
        Err(format!(
            "auto-update rolled back: the {after} build failed the smoke-test; \
             restored the previous binary ({before})"
        )
        .into())
    }
}

/// Run the freshly-installed binary's `version` and confirm it reports
/// the `expected_short` SHA we just pulled — proof the new binary RUNS
/// and is the build we intended. Any failure (won't run, wrong/old SHA,
/// unparseable) returns false → the caller rolls back.
fn smoke_test_new_binary(airc_exe: &Path, expected_short: &str) -> bool {
    let Ok(output) = Command::new(airc_exe).arg("version").output() else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    match parse_build_sha(&stdout) {
        Some(sha) => smoke_sha_matches(&sha, expected_short),
        None => false,
    }
}

/// The SHA the CURRENTLY INSTALLED binary reports, read by running it. `None`
/// when it won't run or predates the `build:` banner — an old binary being
/// exactly the case where an update matters most, so callers must treat `None`
/// as "unknown", never as "unchanged".
fn installed_binary_sha(airc_exe: &Path) -> Option<String> {
    let output = Command::new(airc_exe).arg("version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    parse_build_sha(&String::from_utf8_lossy(&output.stdout))
}

/// The one line the operator reads. Speaks about the BINARY; mentions the
/// checkout only when it is the thing that did not move.
///
/// `binary_before` is `None` when the old binary could not report its SHA. That
/// is NOT "unchanged" — an unreportable binary is precisely the stale one — so
/// it renders as an installed-at statement rather than an "Already at" that
/// would claim a no-op we cannot substantiate.
///
/// Pure — unit-tested.
fn update_summary(
    binary_before: Option<&str>,
    source_before: &str,
    after: &str,
    channel: &str,
) -> String {
    match binary_before {
        Some(b) if smoke_sha_matches(b, after) => {
            // Binary already correct. Say whether the checkout moved under it,
            // because "nothing to do" and "the source advanced but the binary
            // was already built from it" are different facts to an operator
            // debugging a stale node.
            if source_before == after {
                format!("Already at {after} on channel {channel} (binary and source current).")
            } else {
                format!(
                    "Already at {after} on channel {channel} (binary current; source {source_before} -> {after})."
                )
            }
        }
        Some(b) => format!("Updated ({channel}): binary {b} -> {after}"),
        None => format!(
            "Installed ({channel}): binary now {after} (previous build unknown — it could not report one)"
        ),
    }
}

/// Parse the build SHA from `airc version` output (the token after the
/// `build:` label). Pure — unit-tested.
fn parse_build_sha(version_stdout: &str) -> Option<String> {
    for line in version_stdout.lines() {
        if let Some(rest) = line.trim().strip_prefix("build:") {
            return rest.split_whitespace().next().map(|s| s.to_string());
        }
    }
    None
}

/// Whether the installed binary's SHA matches the pulled short SHA.
/// Tolerant of differing short-SHA lengths (git `--short` vs the version
/// banner's 12-char form) via a prefix match either direction.
fn smoke_sha_matches(installed_sha: &str, expected_short: &str) -> bool {
    !installed_sha.is_empty()
        && !expected_short.is_empty()
        && (installed_sha.starts_with(expected_short) || expected_short.starts_with(installed_sha))
}

fn daemon_is_running(
    airc_exe: &Path,
    home: &Path,
    socket: &Path,
) -> Result<bool, Box<dyn std::error::Error>> {
    Ok(daemon_command(airc_exe, home, "ping", socket)
        .output()?
        .status
        .success())
}

fn stop_daemon(
    airc_exe: &Path,
    home: &Path,
    socket: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut command = daemon_command(airc_exe, home, "stop", socket);
    run_checked(&mut command, "airc stop before update")
}

fn restart_daemon(
    airc_exe: &Path,
    home: &Path,
    socket: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(home)?;
    let log = home.join("airc-daemon.log");
    let stdout = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)?;
    let stderr = stdout.try_clone()?;
    let mut command = daemon_command(airc_exe, home, "daemon", socket);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    detach_daemon(&mut command);
    command.spawn()?;
    Ok(())
}

fn wait_daemon_ready(
    airc_exe: &Path,
    home: &Path,
    socket: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    // what this catches: the same cold-start-too-short bug #1211 fixed
    // for `ensure_daemon_running` (5s → 20s). A freshly rebuilt daemon
    // re-runs SQLite migrations + identity load + substrate `Airc::open`
    // on boot before it binds its IPC socket; on a cold/slow machine
    // that exceeds 5s, so `airc update` reported "daemon did not become
    // ready" even though the daemon came up moments later — leaving the
    // node on the OLD build. 20s comfortably covers a cold boot while
    // still surfacing a genuinely dead daemon.
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if daemon_is_running(airc_exe, home, socket)? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "daemon did not become ready after update: {}",
                home.display()
            )
            .into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn daemon_command(airc_exe: &Path, home: &Path, subcommand: &str, socket: &Path) -> Command {
    let mut command = Command::new(airc_exe);
    command
        .arg("--home")
        .arg(home)
        .arg(subcommand)
        .arg("--socket")
        .arg(socket);
    command
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

#[cfg(not(unix))]
fn detach_daemon(_command: &mut Command) {}

pub(crate) fn install_source_dir() -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Some(path) = env::var_os("AIRC_DIR").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    let home = env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .ok_or("HOME is not set; cannot resolve the airc install source")?;
    let home = PathBuf::from(home);
    // install.sh / install.ps1 record the dir they actually installed
    // FROM in this marker. install.sh's _default_clone_dir installs from a
    // dev checkout (e.g. ~/work/airc) when run inside one — and
    // rust-rewrite currently ships ONLY as a dev checkout (no release
    // channel yet) — so the source is frequently NOT ~/.airc/src. Without
    // the marker, `airc update` died with "No git checkout at
    // ~/.airc/src" for every dev-checkout install (caught live
    // 2026-06-13). Honor the marker before falling back to the default.
    if let Some(recorded) = read_install_source_marker(&home) {
        return Ok(recorded);
    }
    Ok(home.join(".airc").join("src"))
}

/// Read the path recorded in `~/.airc/install-source`, if present and
/// non-empty. Returns `None` on any read error or blank content so the
/// caller falls back to the default location.
fn read_install_source_marker(home: &Path) -> Option<PathBuf> {
    let marker = home.join(".airc").join("install-source");
    let contents = std::fs::read_to_string(&marker).ok()?;
    let trimmed = contents.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(PathBuf::from(trimmed))
}

fn validate_source_checkout(source: &Path) -> Result<(), Box<dyn std::error::Error>> {
    if !source.join(".git").exists() {
        return Err(format!(
            "No git checkout at {}. Reinstall airc from the install script.",
            source.display()
        )
        .into());
    }
    if !source.join("install.sh").is_file() {
        return Err(format!(
            "install source {} is missing install.sh; reinstall airc from the install script",
            source.display()
        )
        .into());
    }
    Ok(())
}

/// Confirm the running daemon is the build we just installed — and if it is
/// not, HEAL it rather than reporting a problem to a human.
///
/// `wait_daemon_ready` proves *a* daemon answers. It does not prove it is the
/// daemon we just built. A process that survived `stop_daemon` answers IPC
/// perfectly, so "daemon: restarted." was true and useless. That is the exact
/// drift `doctor.rs::check_daemon_build` was written for — after it went
/// undetected on a live node for hours — and it only runs under
/// `doctor --health`, which update never invokes.
///
/// Fail-loud is not the goal here. Per the self-healing mandate (#288, "heal
/// through flux and updates autonomously — no human ever runs the fix"), a
/// deploy path that *detects* staleness and hands it back to an operator has
/// only moved the work. The remedy for a stale daemon is known, safe, and
/// idempotent — stop it and start it again — so we do that ourselves, once.
///
/// Only when the second attempt ALSO comes back stale do we stop and say so,
/// naming both shas and what was already tried. One retry, not a loop: a
/// daemon that ignores two clean restarts has something wrong that another
/// restart will not fix, and hiding that behind retries is how a brittle
/// system looks healthy right up until it doesn't.
fn verify_daemon_build(
    airc_exe: &Path,
    home: &Path,
    socket: &Path,
    expected: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if daemon_build_matches(airc_exe, home, socket, expected) {
        return Ok(());
    }

    eprintln!(
        "⚠ the daemon came back on a build that is not {expected} — restarting it \
         once more before reporting anything (a process that survived the stop \
         answers IPC just fine)."
    );
    restart_daemon(airc_exe, home, socket)?;
    wait_daemon_ready(airc_exe, home, socket)?;

    if daemon_build_matches(airc_exe, home, socket, expected) {
        eprintln!("✓ self-healed: the daemon is now running {expected}.");
        return Ok(());
    }

    Err(format!(
        "the running daemon is NOT the build that was just installed ({expected}), \
         and a second clean restart did not change that. The binary on disk is \
         correct — something is holding an old daemon process alive. Check for a \
         stray `airc` process that did not exit, then `airc stop` and `airc join` \
         to re-establish the transport owner. Reporting rather than retrying \
         further: two restarts that both fail is not a timing problem."
    )
    .into())
}

/// Whether the daemon reachable on `socket` reports `expected` as its build.
///
/// `false` when it cannot be asked or reports nothing — an unverifiable daemon
/// is not a verified one, and this is the predicate a heal decision hangs off,
/// so "I don't know" must never read as "fine".
fn daemon_build_matches(airc_exe: &Path, home: &Path, socket: &Path, expected: &str) -> bool {
    let Ok(output) = daemon_command(airc_exe, home, "status", socket).output() else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    match parse_build_sha(&stdout) {
        Some(sha) => smoke_sha_matches(&sha, expected),
        None => false,
    }
}

/// Compile the new build while the daemon is still serving, so the outage
/// covers only the binary swap.
///
/// Mirrors `install.sh`'s own `cargo build --release -p airc-cli` in the same
/// directory, so this populates precisely the cache the installer will hit.
/// Silent on success, one line on failure — and deliberately infallible to the
/// caller: `run_installer` does the authoritative build immediately after and
/// surfaces any real compile error there. Treating a pre-warm failure as fatal
/// would turn an optimization into a new way for `airc update` to refuse.
fn prewarm_build(source: &Path) {
    println!("Pre-building while the daemon stays up (keeps the node reachable)…");
    let status = Command::new("cargo")
        .args(["build", "--release", "-p", "airc-cli"])
        .current_dir(source)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => println!("pre-build exited {s} — the installer will build it properly."),
        Err(e) => println!("pre-build could not run ({e}) — the installer will build it."),
    }
}

fn run_installer(source: &Path) -> Result<(), Box<dyn std::error::Error>> {
    // Stream the installer's output live instead of buffering it with
    // `Command::output()` (what run_checked does). install.sh does the
    // cargo rebuild, which can take minutes; with buffered stdio the
    // operator saw NOTHING while it ran, so a long-or-hung build was
    // indistinguishable from a working one — on a live Windows node
    // `airc update` "hung" silently for 15+ minutes with no visible
    // progress. Inheriting stdio surfaces cargo's progress live; the
    // banner sets the expectation up front. Failure behavior is
    // unchanged: a non-zero exit still returns an Err.
    println!("Rebuilding airc (this can take a few minutes)…");
    let status = Command::new(installer_shell())
        .arg(source.join("install.sh"))
        .env("AIRC_DIR", source)
        .env("AIRC_INSTALL_NO_PULL", "1")
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;
    if status.success() {
        return Ok(());
    }
    Err(format!("install.sh failed: exit status {status}").into())
}

/// The shell used to run install.sh during `airc update`.
///
/// On Windows, a plain `bash` resolves to `C:\Windows\System32\bash.exe`
/// — the WSL launcher — which fails with "Windows Subsystem for Linux has
/// no installed distributions" when no distro is present, so `airc
/// update` died at the reinstall step (caught live 2026-06-13). Prefer
/// the Git-for-Windows bash derived from `git --exec-path` (git is an
/// airc prereq). On Unix there is no `bin/bash.exe`, so this finds
/// nothing and the caller falls back to plain `bash` — unchanged.
fn installer_shell() -> std::ffi::OsString {
    if let Some(bash) = git_bundled_bash() {
        return bash.into_os_string();
    }
    std::ffi::OsString::from("bash")
}

fn git_bundled_bash() -> Option<PathBuf> {
    let output = Command::new("git").arg("--exec-path").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let exec = String::from_utf8(output.stdout).ok()?;
    bash_in_git_root(Path::new(exec.trim()))
}

/// Walk up from a git exec-path (e.g. `.../Git/mingw64/libexec/git-core`)
/// looking for a bundled `bin/bash.exe` or `usr/bin/bash.exe` under any
/// ancestor. Pure path logic so it is testable on every platform.
fn bash_in_git_root(exec_path: &Path) -> Option<PathBuf> {
    exec_path.ancestors().find_map(|root| {
        ["bin/bash.exe", "usr/bin/bash.exe"]
            .iter()
            .map(|rel| root.join(rel))
            .find(|candidate| candidate.is_file())
    })
}

fn git_text<const N: usize>(
    source: &Path,
    args: [&str; N],
) -> Result<String, Box<dyn std::error::Error>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(source)
        .args(args)
        .output()?;
    if !output.status.success() {
        return Err(command_error("git", &output).into());
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

fn run_checked(
    command: &mut Command,
    label: &'static str,
) -> Result<(), Box<dyn std::error::Error>> {
    let output = command.output()?;
    if output.status.success() {
        return Ok(());
    }
    Err(command_error(label, &output).into())
}

fn command_error(label: &str, output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        format!("exit status {}", output.status)
    };
    format!("{label} failed: {detail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// what this catches (#354): the exact live shape that motivated wiring
    /// the smoke-test into the MANUAL update path. On 2026-08-07 a peer's
    /// source resolved to one commit while the binary on PATH still reported
    /// an older one, and `airc update` printed a success line anyway.
    ///
    /// The existing `smoke_sha_matches` tests cover the tolerant-prefix
    /// direction (a successful update must not read as a failure). This covers
    /// the other one: a genuinely stale binary must NOT slip through on a
    /// prefix coincidence, and an unparsed/empty value must never vacuously
    /// satisfy the check — a verification that passes on missing evidence is
    /// worse than none.
    #[test]
    fn a_stale_binary_does_not_satisfy_the_commit_the_source_reached() {
        assert!(!smoke_sha_matches("1e2f424aaaaa", "35d40b1"));
        assert!(!smoke_sha_matches("", "35d40b1"));
        assert!(!smoke_sha_matches("35d40b1468ee", ""));
    }

    // what this catches (#288 pin-to-channel): the durable channel file wins over
    // the default, and missing/empty files fall through to "canary" — the update
    // must NEVER derive its ref from the checkout's current branch again (the
    // deleted-PR-branch brick + off-channel-strand incident, 2026-08-01).
    #[test]
    fn channel_file_wins_missing_or_empty_falls_to_default() {
        let dir = std::env::temp_dir().join(format!("airc-chan-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("update-channel");

        std::fs::write(&file, "release-2\n").unwrap();
        assert_eq!(channel_from_file(&file).as_deref(), Some("release-2"));

        std::fs::write(&file, "   \n").unwrap();
        assert_eq!(channel_from_file(&file), None, "blank file falls through");

        let _ = std::fs::remove_file(&file);
        assert_eq!(channel_from_file(&file), None, "missing file falls through");
        assert_eq!(DEFAULT_UPDATE_CHANNEL, "canary");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_source_prefers_airc_dir() {
        temp_env::with_vars(
            [
                ("AIRC_DIR", Some("/tmp/custom-airc")),
                ("HOME", Some("/tmp/home")),
            ],
            || {
                assert_eq!(
                    install_source_dir().unwrap(),
                    PathBuf::from("/tmp/custom-airc")
                );
            },
        );
    }

    #[test]
    fn install_source_defaults_to_home_airc_src() {
        temp_env::with_vars(
            [
                ("AIRC_DIR", None::<&str>),
                ("HOME", Some("/tmp/home")),
                ("USERPROFILE", Some("/tmp/userprofile")),
            ],
            || {
                assert_eq!(
                    install_source_dir().unwrap(),
                    PathBuf::from("/tmp/home/.airc/src")
                );
            },
        );
    }

    #[test]
    fn install_source_reads_marker_when_no_env() {
        // what this catches: airc update finding a dev-checkout install
        // source recorded by install.sh, instead of dying on ~/.airc/src
        // (regression for the 2026-06-13 "No git checkout" dev-install bug).
        let temp = tempfile::TempDir::new().unwrap();
        let airc = temp.path().join(".airc");
        std::fs::create_dir_all(&airc).unwrap();
        std::fs::write(airc.join("install-source"), "/opt/dev/airc\n").unwrap();
        temp_env::with_vars(
            [
                ("AIRC_DIR", None::<&str>),
                ("HOME", Some(temp.path().to_str().unwrap())),
                ("USERPROFILE", None::<&str>),
            ],
            || {
                assert_eq!(
                    install_source_dir().unwrap(),
                    PathBuf::from("/opt/dev/airc")
                );
            },
        );
    }

    #[test]
    fn install_source_airc_dir_beats_marker() {
        // what this catches: explicit AIRC_DIR must still win over a
        // recorded marker (precedence order regression).
        let temp = tempfile::TempDir::new().unwrap();
        let airc = temp.path().join(".airc");
        std::fs::create_dir_all(&airc).unwrap();
        std::fs::write(airc.join("install-source"), "/opt/dev/airc\n").unwrap();
        temp_env::with_vars(
            [
                ("AIRC_DIR", Some("/explicit/override")),
                ("HOME", Some(temp.path().to_str().unwrap())),
            ],
            || {
                assert_eq!(
                    install_source_dir().unwrap(),
                    PathBuf::from("/explicit/override")
                );
            },
        );
    }

    #[test]
    fn install_source_blank_marker_falls_back_to_default() {
        // what this catches: a blank/whitespace marker must not resolve to
        // an empty path; fall through to ~/.airc/src.
        let temp = tempfile::TempDir::new().unwrap();
        let airc = temp.path().join(".airc");
        std::fs::create_dir_all(&airc).unwrap();
        std::fs::write(airc.join("install-source"), "  \n").unwrap();
        temp_env::with_vars(
            [
                ("AIRC_DIR", None::<&str>),
                ("HOME", Some(temp.path().to_str().unwrap())),
                ("USERPROFILE", None::<&str>),
            ],
            || {
                assert_eq!(
                    install_source_dir().unwrap(),
                    temp.path().join(".airc").join("src")
                );
            },
        );
    }

    #[test]
    fn bash_in_git_root_finds_bundled_bash() {
        // what this catches: airc update finding Git-for-Windows' bash via
        // git --exec-path instead of invoking the System32 WSL launcher
        // (regression for the 2026-06-13 "WSL has no distributions" bug).
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path();
        std::fs::create_dir_all(root.join("mingw64").join("libexec").join("git-core")).unwrap();
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::write(root.join("bin").join("bash.exe"), b"#!/bin/sh\n").unwrap();
        let exec = root.join("mingw64").join("libexec").join("git-core");
        assert_eq!(
            bash_in_git_root(&exec).unwrap(),
            root.join("bin").join("bash.exe")
        );
    }

    #[test]
    fn bash_in_git_root_none_when_absent() {
        // what this catches: no false positive when no bundled bash exists,
        // so installer_shell falls back to plain `bash` (Unix path).
        let temp = tempfile::TempDir::new().unwrap();
        let exec = temp.path().join("mingw64").join("libexec").join("git-core");
        assert!(bash_in_git_root(&exec).is_none());
    }

    #[test]
    fn validate_source_requires_git_checkout() {
        let temp = tempfile::TempDir::new().unwrap();
        let error = validate_source_checkout(temp.path())
            .unwrap_err()
            .to_string();
        assert!(error.contains("No git checkout"));
    }

    #[test]
    fn daemon_command_passes_home_subcommand_and_socket() {
        let command = daemon_command(
            Path::new("/bin/airc"),
            Path::new("/tmp/home/.airc"),
            "daemon",
            Path::new("/tmp/airc.sock"),
        );
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert_eq!(
            args,
            vec![
                "--home",
                "/tmp/home/.airc",
                "daemon",
                "--socket",
                "/tmp/airc.sock"
            ]
        );
    }

    // what this catches: the smoke-test parses the build SHA from real
    // `airc version` output (the `build:` line), so a successful rebuild
    // can be verified to actually BE the build we pulled.
    #[test]
    fn parse_build_sha_reads_the_version_banner() {
        let out =
            "  airc 0.1.0\n  install: /home/u/.local/bin/airc\n  build:   9cc678fc0203 on canary\n";
        assert_eq!(parse_build_sha(out).as_deref(), Some("9cc678fc0203"));
        assert_eq!(parse_build_sha("no build line here"), None);
    }

    // what this catches: the summary must describe the BINARY, because the two
    // most different outcomes used to print the identical line. Regression for
    // the live BigMama case on 2026-08-07, immediately after #354: the checkout
    // was ALREADY at a08f3d3 (a manual pull had moved it) while the installed
    // binary was still 35d40b1, so `before == after` held, the binary was
    // genuinely replaced, and update printed "Already at" — which an operator
    // reads as "no work done" and acts on by not restarting anything.
    #[test]
    fn summary_reports_the_binary_transition_not_the_checkouts() {
        // THE live case: source already current, binary stale → an UPDATE.
        let s = update_summary(Some("35d40b1468ee"), "a08f3d3", "a08f3d3", "canary");
        assert!(
            s.starts_with("Updated (canary): binary 35d40b1468ee -> a08f3d3"),
            "a replaced binary must not read as a no-op: {s}"
        );
        assert!(!s.contains("Already at"), "the old wording is the bug: {s}");

        // Genuine no-op: binary already the pulled SHA and source did not move.
        let noop = update_summary(Some("a08f3d38145c"), "a08f3d3", "a08f3d3", "canary");
        assert!(noop.starts_with("Already at a08f3d3"), "{noop}");
        assert!(noop.contains("binary and source current"), "{noop}");

        // Source advanced but the binary was already built from the new tip —
        // distinct from a no-op for anyone debugging a stale node.
        let caught_up = update_summary(Some("a08f3d38145c"), "35d40b1", "a08f3d3", "canary");
        assert!(
            caught_up.contains("source 35d40b1 -> a08f3d3"),
            "{caught_up}"
        );

        // Unreportable previous build is UNKNOWN, never "unchanged" — an old
        // binary with no `build:` banner is exactly the one needing an update.
        let unknown = update_summary(None, "35d40b1", "a08f3d3", "canary");
        assert!(
            !unknown.contains("Already at"),
            "unknown != no-op: {unknown}"
        );
        assert!(unknown.contains("previous build unknown"), "{unknown}");

        // Short-vs-long SHA forms must still compare equal (same tolerance the
        // smoke test uses), or every update would report itself as a change.
        let mixed = update_summary(Some("a08f3d38145c"), "a08f3d3", "a08f3d3", "canary");
        assert!(
            mixed.starts_with("Already at"),
            "prefix match holds: {mixed}"
        );
    }

    // what this catches: the SHA match tolerates the differing short-SHA
    // lengths (git --short ~7 chars vs the 12-char version banner) via a
    // prefix match either direction — and rejects a mismatch (the
    // rollback trigger) and empties.
    #[test]
    fn smoke_sha_matches_tolerates_short_sha_lengths() {
        assert!(smoke_sha_matches("9cc678fc0203", "9cc678f")); // banner longer
        assert!(smoke_sha_matches("9cc678f", "9cc678fc0203")); // pulled longer
        assert!(smoke_sha_matches("abcd1234", "abcd1234"));
        assert!(!smoke_sha_matches("9cc678fc0203", "deadbeef")); // mismatch -> rollback
        assert!(!smoke_sha_matches("", "9cc678f"));
        assert!(!smoke_sha_matches("9cc678f", ""));
    }
}
