# Prebuilt updater dependency (card 07ed7280)

Windows, Fable M5 and Cormac Intel remained installed/running at 4a5024be5f76
when canary advanced to 160a6a74. No published releases or Actions artifacts
were available. Existing release.yml was dispatched on canary as build-only
run 37792458017; this does not publish a release.

The public updater prepared by compiling even when CI could supply an artifact.
The manual installer prebuilt phase assumes a caller-owned maintenance window,
so it was not safe to substitute a live copy/restart. No such workaround ran.

The existing install.sh prepare owner now accepts explicit artifact+SHA256,
copies into the existing private snapshot, checks bytes before execution and
checks expected build. Existing PreparedInstall and native session adapters
remain the only maintenance/publication/rollback owners. No second updater,
new daemon, source reset, or Cargo fallback is added. Compile prerequisites are
not acquired for this explicit path.

The existing update-handoff fixture retains source-build/metadata/refusal and
adapter coverage and now exercises malformed/wrong checksum, wrong revision,
no compilation fallback, and input replacement after preparation. Generic
contract runs once per fixture; native Windows adapter remains in that fixture.

Installed adoption is OPEN until a verified prebuilt runs through public update
and both installed and daemon revisions match. Automatic discovery/download is
also still OPEN: explicit artifact input is a supported bridge, not the full
end-user release installer. Do not close either gap based only on tests.

Validation: existing handoff fixture passes Windows (6 passed, 2 ignored native probes) and macOS (2 passed). Windows airc-cli all-targets Clippy with warnings denied and workspace fmt check pass. No runtime adoption performed.

## Actual adoption, 2026-10-08 (follow-up card 476c2b2d)

Build-only run37797750932 supplied revision44cc2a515ad3. Windows verified
archive SHA256, then binary F8113ECE8DBA5C1EB5BD6814819558ADA9592F016FEAB8A2D5EA29D9BA877FC4.
Public update with explicit prebuilt input succeeded: CLI and daemon both
44cc2a515ad3, same peer identity; feature checkout remained clean and unchanged.
No local compilation occurred.

Fable M5 verified the matching ARM archive and binary, but installation failed
in Codex hook setup: cargo metadata was unavailable in SSH PATH. The updater
restored4a5024be5f76; automatic recovery timed out. A single supported
`update --adopt-installed` then restored the old daemon and verified its build.
Mac adoption remains OPEN; no direct copy or manual stop/start was used.

Fix: one `_installed_airc_binary` selector now owns hook/token/rules integration
selection; all three callers consume the verified installed binary. The prior
Cargo-output lookup and two duplicate selectors were deleted. Existing handoff
coverage now enables hook setup with deliberately broken metadata and stale
Cargo outputs. Windows6pass/2ignored and Mac2pass. This must still be exercised
through a normal update before the Mac installer gap can close.


## Installed acceptance, 2026-10-08 (card ed2598ed)

All three installed CLIs and production daemons report `1f9a78793461`:
Windows peer e85a5bb3, M5 peer 2f0aed7f, Intel peer 5159a48b. Windows
returned to58connections at81seconds uptime; M5 retained703 at458seconds;
Intel reported71 at257seconds. Continuum core PIDs80117 (M5) and96297 (Intel)
were unchanged. Windows feature checkout stayed clean on feat/msg-to-peer.

Windows consumed the checksum-verified CI artifact from build-only run
37807242725 through normal `airc update` with explicit prebuilt input. Installed
and daemon revision were verified; no local compilation. Its saved409-character
Codex coordination brief and600second cadence remained unchanged. Earlier live
CLI consumer proof verified busy suppression,2467byte first delivery and0byte
immediate repeat. This proves delivery/cursor behavior, not agent action.

Both Macs independently advanced to source-build-labelled HEAD revision1f9a787
while CI artifacts were building (build1111,16:19Z). The later M5 public update
therefore returned a verified no-op. Their successful transitions must not be
attributed to this operator's prebuilt path. Mac prebuilt transaction provenance
remains open; neither Mac brief was configured (both null). No redundant
production restart was performed to manufacture evidence.

The installed M5 binary was separately invoked in a fresh, explicit temporary
home from ordinary SSH soft limit256. Its typed startup diagnostic reported
before_soft256,actual_soft4096,hard unlimited; status answered with revision
1f9a787. That isolated proof daemon was stopped and its process absence checked.
Production remained serving703connections. This closes the installed Unix
capacity adapter observation; it is not a claim about the independent updater's
launch environment (production startup reported4096before and after).

Raw evidence retained under the team-proof state directory:
20261008-airc-windows-1f9-update.log,
20261008-m5-installed-capacity.log,
20261008-m5-installed-capacity-status.txt,
20261008-airc-resume-first.txt and20261008-airc-resume-repeat.txt.
Automatic artifact discovery/download is still open; explicit verified input
is the supported bridge. No runtime audio/video harness was involved.


Final M5 `doctor --health` clarifies the connection counts: its daemon snapshot
was1second old, with **no connected LAN peers and an empty delivery ledger**.
The703 connections above are IPC liveness, not confirmed mesh delivery. The
store warning was1073MB plus124MB WAL. Matching builds and capacity recovery
must not be presented as grid transport health. Evidence:
20261008-m5-airc-health.txt. No blind doctor --fix or state cleanup was run.

Windows and Intel route snapshots do show recent acknowledged peer delivery:
Windows→Intel83/83 acknowledgments, RTT81ms,41seconds old; Intel→Windows180/181,
RTT23ms,14seconds old. Each had2 connected LAN peers and also acknowledged
peer0121d959. These are transport acknowledgments, not proof an agent read or
acted on a message. The empty-route gap is specifically M5 in these samples.

Follow-up diagnosis: the M5 GitHub CLI reports its active joelteply credential
invalid, and public registry sync skips unauthenticated even with Homebrew PATH.
The Windows address book had M5 port50189 while its live daemon advertises57958.
The existing key/tier was preserved while updating that endpoint through peer add;
manual authenticated dial succeeded in both directions. These short-lived CLI
handshakes prove reachability, not sustained daemon routing. No credential was
copied, no core restarted, and no store was edited directly. Durable discovery
still requires credential recovery; daemon ACK acceptance remains open.
