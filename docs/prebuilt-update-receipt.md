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
