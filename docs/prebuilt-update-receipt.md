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
